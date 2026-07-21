using System.Text.Json;
using System.Text.Json.Nodes;
using Mvf.Graph.Control;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Simulation;
using Mvf.Abstractions;
using Mvf.Engine.Execution.NodeRunners;

namespace Mvf.Engine.Execution;

/// <summary>
/// Resolves a <see cref="PipelineNodeDefinition"/> into an activated <see cref="INodeRunner"/>.
///
/// Resolution rules (by node kind):
/// <list type="bullet">
///   <item><c>integration-module</c> — finds the module by ModuleId, casts to the correct interface</item>
///   <item><c>embedded-primitive</c> — creates a built-in runner for <c>if</c>, <c>switch</c>, etc.</item>
///   <item><c>runtime-builtin</c> — creates a built-in runner for dataset-writer, folder-sequence-source, etc.</item>
/// </list>
/// </summary>
public sealed class PipelineNodeActivator(
    IIntegrationModuleLoader integrationModuleLoader,
    ISimulatorSourceCatalog simulatorSourceCatalog) : IPipelineNodeActivator
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    public async Task<INodeRunner> ActivateAsync(
        PipelineNodeDefinition node,
        PipelineExecutionOptions options,
        CancellationToken cancellationToken)
    {
        INodeRunner runner = node.Kind switch
        {
            "integration-module" => CreateIntegrationModuleRunner(node, options),
            "embedded-primitive" => CreateEmbeddedPrimitiveRunner(node),
            "runtime-builtin" => CreateRuntimeBuiltinRunner(node, options),
            _ => throw new InvalidOperationException(
                $"Node '{node.Id}' has unknown kind '{node.Kind}'. " +
                $"Expected one of: integration-module, embedded-primitive, runtime-builtin.")
        };

        await runner.ActivateAsync(cancellationToken);
        return runner;
    }

    private INodeRunner CreateIntegrationModuleRunner(PipelineNodeDefinition node, PipelineExecutionOptions options)
    {
        if (string.IsNullOrWhiteSpace(node.ModuleId))
        {
            throw new InvalidOperationException(
                $"Integration-module node '{node.Id}' is missing ModuleId.");
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

    private static INodeRunner CreateEmbeddedPrimitiveRunner(PipelineNodeDefinition node)
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

            _ => throw new InvalidOperationException(
                $"Node '{node.Id}' references unknown embedded primitive type '{node.PrimitiveType}'. " +
                $"Supported types: if, fork, switch.")
        };
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
