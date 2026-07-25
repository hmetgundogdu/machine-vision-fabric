using System.Text.Json;
using System.Text.Json.Nodes;
using Mvf.Graph.Control;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Simulation;
using Mvf.Graph.Values;
using Mvf.Abstractions;
using Mvf.Engine.Execution.NodeRunners;
using Mvf.Engine.Modules;
using Mvf.Engine.Values;

namespace Mvf.Engine.Execution;

/// <summary>
/// Resolves a <see cref="PipelineNodeDefinition"/> into an activated <see cref="INodeRunner"/>.
///
/// Resolution rules (by node kind):
/// <list type="bullet">
///   <item><c>integration-module</c> — finds the module by ModuleId, casts to the correct interface.
///     A non-<c>dotnet</c> runtime (Python/Node) is hosted out-of-process via
///     <see cref="IOutOfProcessModuleHost"/> instead of being loaded as an assembly.</item>
///   <item><c>embedded-primitive</c> — creates a built-in runner for <c>if</c>, <c>switch</c>, etc.</item>
///   <item><c>runtime-builtin</c> — creates a built-in runner for dataset-writer, folder-sequence-source, etc.</item>
/// </list>
/// </summary>
public sealed class PipelineNodeActivator(
    IIntegrationModuleLoader integrationModuleLoader,
    ISimulatorSourceCatalog simulatorSourceCatalog,
    ModuleCatalog moduleCatalog,
    IOutOfProcessModuleHost? outOfProcessModuleHost = null,
    LiveValueRegistry? liveValues = null) : IPipelineNodeActivator
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    public async Task<INodeRunner> ActivateAsync(
        PipelineNodeDefinition node,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken)
    {
        INodeRunner runner = node.Kind switch
        {
            "integration-module" => await CreateIntegrationModuleRunnerAsync(node, options, cancellationToken),
            "embedded-primitive" => CreateEmbeddedPrimitiveRunner(node),
            "runtime-builtin" => CreateRuntimeBuiltinRunner(node, options),
            _ => throw new InvalidOperationException(
                $"Node '{node.Id}' has unknown kind '{node.Kind}'. " +
                $"Expected one of: integration-module, embedded-primitive, runtime-builtin.")
        };

        RegisterModuleBindings(node);
        await runner.ActivateAsync(cancellationToken);
        return runner;
    }

    /// <summary>
    /// Registers each of a node's live-editable config fields as a tunable, so the CLI can offer it. Its
    /// current value is whatever is in config right now (the pre-pass has already overlaid any stored edit).
    /// Editing it is what triggers re-activation — the executor watches the registry and re-opens the node
    /// with the new value, because a module reads its config only at activation. Re-registering on
    /// re-activation simply refreshes the entry with the new value.
    /// </summary>
    private void RegisterModuleBindings(PipelineNodeDefinition node)
    {
        if (liveValues is null || node.Bindings.Count == 0)
        {
            return;
        }

        foreach (var binding in ModuleBindings.Read(node.Bindings))
        {
            if (!binding.TypeKnown)
            {
                continue;   // the validator reports an unknown-typed binding; do not register a broken one
            }

            var current = node.Config.TryGetPropertyValue(binding.Field, out var value) ? value?.DeepClone() : null;

            liveValues.Register(
                binding.LiveKey(node.Id),
                binding.Prompt ?? $"{node.DisplayName} · {binding.Field}",
                binding.Type,
                binding.Schema,
                binding.Binding,
                current,
                ControlValueShape.Single);
        }
    }

    private async Task<INodeRunner> CreateIntegrationModuleRunnerAsync(
        PipelineNodeDefinition node,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken)
    {
        if (string.IsNullOrWhiteSpace(node.ModuleId))
        {
            throw new InvalidOperationException(
                $"Integration-module node '{node.Id}' is missing ModuleId.");
        }

        // A non-.NET module is hosted out-of-process. Its runtime is read from the manifest
        // catalog (metadata only, no assembly load), so we never try to load it as a plugin.
        var catalog = moduleCatalog.Load(options.IntegrationsRoot);
        if (catalog.TryGetValue(node.ModuleId, out var entry)
            && !string.Equals(entry.Manifest.Runtime, "dotnet", StringComparison.OrdinalIgnoreCase))
        {
            return await CreateOutOfProcessRunnerAsync(node, entry, options, cancellationToken);
        }

        var modules = integrationModuleLoader.LoadModules(options.IntegrationsRoot);
        var module = modules.FirstOrDefault(m =>
            string.Equals(m.Describe().ModuleId, node.ModuleId, StringComparison.OrdinalIgnoreCase));

        if (module is null)
        {
            throw new InvalidOperationException(
                $"Integration module '{node.ModuleId}' could not be found under '{options.IntegrationsRoot}'.");
        }

        var config = ToJsonElement(node.Config);

        return node.Category switch
        {
            "source" when module is IFrameSourceModule sourceModule =>
                new FrameSourceNodeRunner(node.Id, sourceModule.OpenSession(config, options.PackageRoot)),

            "control" when module is IProductPresenceGateModule gateModule =>
                new ProductPresenceGateNodeRunner(node.Id, gateModule.CreateGate(config)),

            "compute" when module is IFrameProcessorModule processorModule =>
                new FrameProcessorNodeRunner(node.Id, processorModule.CreateProcessor(config)),

            "classify" when module is IFrameClassifierModule classifierModule =>
                new FrameClassifierNodeRunner(node.Id, classifierModule.CreateClassifier(config)),

            "output" or "sink" when module is IFrameSinkModule sinkModule =>
                new FrameSinkNodeRunner(node.Id, sinkModule.OpenSink(config, options.PackageRoot)),

            _ => throw new InvalidOperationException(
                $"Node '{node.Id}' has category '{node.Category}' but the loaded module type is not compatible.")
        };
    }

    private async Task<INodeRunner> CreateOutOfProcessRunnerAsync(
        PipelineNodeDefinition node,
        ModuleCatalogEntry entry,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken)
    {
        if (outOfProcessModuleHost is null)
        {
            throw new InvalidOperationException(
                $"Node '{node.Id}' uses module '{entry.Manifest.Id}' (runtime '{entry.Manifest.Runtime}'), " +
                "which needs an out-of-process module host, but none is registered.");
        }

        // Bind the run-level log sink to this node: the worker only knows its own module id, so the
        // engine attaches the node id here on the way up. Null when no one is listening (headless with
        // no callback) keeps the worker's stdout/stderr draining but forwards nothing.
        var onNodeLog = options.OnNodeLog;
        Action<WorkerLogLine>? onLog = onNodeLog is null
            ? null
            : line => onNodeLog(new NodeLogEvent
            {
                NodeId = node.Id,
                ModuleId = entry.Manifest.Id,
                Level = line.Level,
                Message = line.Message
            });

        var activation = new OutOfProcessModuleActivation(
            ModuleId: entry.Manifest.Id,
            Runtime: entry.Manifest.Runtime,
            EntryPath: Path.Combine(entry.Directory, entry.Manifest.Entry),
            WorkingDirectory: entry.Directory,
            OnLog: onLog);

        return node.Category switch
        {
            "classify" => new FrameClassifierNodeRunner(
                node.Id,
                await outOfProcessModuleHost.CreateClassifierAsync(activation, cancellationToken)),

            "compute" => new FrameTransformerNodeRunner(
                node.Id,
                await outOfProcessModuleHost.CreateTransformerAsync(activation, cancellationToken)),

            _ => throw new InvalidOperationException(
                $"Out-of-process module '{entry.Manifest.Id}' (node '{node.Id}') supports the 'classify' and " +
                $"'compute' categories, not '{node.Category}'. Other capabilities arrive in later slices.")
        };
    }

    private INodeRunner CreateEmbeddedPrimitiveRunner(PipelineNodeDefinition node)
    {
        return node.PrimitiveType switch
        {
            "if" => new IfPrimitiveNodeRunner(node.Id),

            "fork" => new ForkPrimitiveNodeRunner(
                node.Id,
                node.Outputs.Select(p => p.Name).ToList()),

            "switch" => new SwitchPrimitiveNodeRunner(
                node.Id,
                node.Outputs.Select(p => p.Name).ToHashSet(StringComparer.OrdinalIgnoreCase)),

            "value" => CreateValueRunner(node),

            "select" => CreateSelectRunner(node),

            "loop" => new LoopPrimitiveNodeRunner(node.Id),

            _ => throw new InvalidOperationException(
                $"Node '{node.Id}' references unknown embedded primitive type '{node.PrimitiveType}'. " +
                $"Supported types: if, fork, switch, value, select, loop.")
        };
    }

    /// <summary>
    /// A <c>value</c> node is a constant by the time it activates — the binding pre-pass resolved it
    /// before cycle 0. Reaching activation unresolved means the pre-pass was skipped or could not answer,
    /// and failing here is the point: an unattended run must stop with a name to fix, not block on a
    /// human who is not there.
    /// </summary>
    private INodeRunner CreateValueRunner(PipelineNodeDefinition node)
    {
        var config = ValuePrimitiveConfig.Read(node.Config);

        if (!config.TryGetStaticValue(out var value))
        {
            var binding = config.Binding is { Length: > 0 } name
                ? $" Set binding '{name}' (in .mvf/bindings.json) or run the pipeline once interactively."
                : " Add a literal, a binding or a default to the node's config.";

            throw new InvalidOperationException(
                $"Value node '{node.Id}' has no resolved value.{binding}");
        }

        // Registered so a host can offer it as a live tunable. With no registry the value is simply a
        // constant held for the run, which is what every non-interactive host wants.
        var registry = liveValues ?? new LiveValueRegistry();
        var live = registry.Register(
            node.Id,
            config.Prompt ?? node.DisplayName,
            config.Type,
            config.Schema,
            config.Binding,
            value,
            config.Shape);

        return new ValuePrimitiveNodeRunner(node.Id, live);
    }

    /// <summary>
    /// A <c>select</c> may legitimately activate without a criterion: one arriving on the
    /// <c>criterion</c> port is a per-frame criterion, and only the cycle knows it.
    ///
    /// <para>When the criterion is <b>not</b> fed by an edge it becomes a live tunable, and a picking one
    /// at that: the runner publishes the collection it is narrowing, so an operator can re-choose from the
    /// current candidates mid-run instead of typing an identifier read off another screen.</para>
    /// </summary>
    private INodeRunner CreateSelectRunner(PipelineNodeDefinition node)
    {
        var config = SelectPrimitiveConfig.Read(node.Config);
        config.TryGetStaticCriterion(out var criterion);

        if (config.CriterionFromEdge)
        {
            return new SelectPrimitiveNodeRunner(node.Id, config.Mode, criterion, config.By);
        }

        var registry = liveValues ?? new LiveValueRegistry();
        var live = registry.Register(
            node.Id,
            config.Prompt ?? node.DisplayName,
            // With a `by` the criterion is one property of an element, not the element itself.
            config.By is { Length: > 0 } ? ControlValueType.String : config.Type,
            schema: null,
            config.Binding,
            criterion,
            ControlValueShape.Single,
            config.By);

        return new SelectPrimitiveNodeRunner(node.Id, config.Mode, criterion, config.By, live);
    }

    private INodeRunner CreateRuntimeBuiltinRunner(PipelineNodeDefinition node, PipelineExecutionOptions options)
    {
        return node.BuiltinType switch
        {
            "folder-sequence-source" => CreateBuiltinFolderSource(node, options),
            "simulated-product-presence-gate" => CreateBuiltinSimulatedGate(node),
            "dataset-writer" => throw new InvalidOperationException(
                $"Node '{node.Id}': 'dataset-writer' is no longer a runtime-builtin. " +
                $"Use an integration-module node with moduleId 'mvf.dataset-writer' instead."),
            _ => throw new InvalidOperationException(
                $"Node '{node.Id}' references unknown runtime-builtin type '{node.BuiltinType}'. " +
                $"Supported types: folder-sequence-source, simulated-product-presence-gate.")
        };
    }

    private INodeRunner CreateBuiltinFolderSource(PipelineNodeDefinition node, PipelineExecutionOptions options)
    {
        FolderSequenceSourceOptions sourceOptions;
        try
        {
            sourceOptions = node.Config.Count > 0
                ? JsonSerializer.Deserialize<FolderSequenceSourceOptions>(node.Config.ToJsonString(), JsonOptions)
                  ?? new FolderSequenceSourceOptions()
                : new FolderSequenceSourceOptions();
        }
        catch (JsonException)
        {
            sourceOptions = new FolderSequenceSourceOptions();
        }

        // Resolve relative source folder against package root
        if (!string.IsNullOrWhiteSpace(sourceOptions.SourceFolder) && !Path.IsPathRooted(sourceOptions.SourceFolder))
        {
            sourceOptions.SourceFolder = Path.GetFullPath(Path.Combine(options.PackageRoot, sourceOptions.SourceFolder));
        }

        var session = simulatorSourceCatalog.OpenSession(sourceOptions);
        return new FrameSourceNodeRunner(node.Id, session);
    }

    private static INodeRunner CreateBuiltinSimulatedGate(PipelineNodeDefinition node)
    {
        SimulatedPlcGateOptions gateOptions;
        try
        {
            gateOptions = node.Config.Count > 0
                ? JsonSerializer.Deserialize<SimulatedPlcGateOptions>(node.Config.ToJsonString(), JsonOptions)
                  ?? new SimulatedPlcGateOptions()
                : new SimulatedPlcGateOptions();
        }
        catch (JsonException)
        {
            gateOptions = new SimulatedPlcGateOptions();
        }

        var gate = new SimulatedProductPresenceGate(gateOptions);
        return new ProductPresenceGateNodeRunner(node.Id, gate);
    }

    private static JsonElement ToJsonElement(JsonObject obj)
    {
        if (obj.Count == 0)
        {
            return default;
        }

        using var doc = JsonDocument.Parse(obj.ToJsonString());
        return doc.RootElement.Clone();
    }
}
