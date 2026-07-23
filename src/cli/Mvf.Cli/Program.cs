using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Schema;
using System.Text.Json.Serialization;
using System.Text.Json.Serialization.Metadata;
using Mvf.Cli.Tui;
using Mvf.Graph.Control;
using Mvf.Graph.Dataset;
using Mvf.Graph.Execution;
using Mvf.Graph.Integrations;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Simulation;
using Mvf.Abstractions;
using Mvf.Engine;
using Mvf.Engine.Execution;
using Spectre.Console;
using Mvf.Engine.Modules;
using Mvf.Engine.Pipelines;
using Mvf.Engine.Plugins;
using Mvf.Transport.SharedMemory;
using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;

var jsonOptions = new JsonSerializerOptions(JsonSerializerDefaults.Web)
{
    WriteIndented = true
};

var invocation = CliInvocation.Parse(args);
if (invocation.ShowHelp)
{
    PrintHelp();
    return;
}

switch (invocation.Command)
{
    case "packages":
        await ListPackagesAsync(invocation);
        break;
    case "modules":
        await ListModulesAsync(invocation);
        break;
    case "sessions":
        await ListSessionsAsync(invocation);
        break;
    case "inspect-session":
        await InspectSessionAsync(invocation);
        break;
    case "schemas":
        await ExportSchemasAsync(invocation);
        break;
    case "validate-pipeline":
        await ValidatePipelineAsync(invocation);
        break;
    case "execute-graph":
        await ExecuteGraphAsync(invocation);
        break;
    default:
        Console.Error.WriteLine($"Unknown command '{invocation.Command}'.");
        PrintHelp();
        Environment.ExitCode = 1;
        break;
}

return;

Task ListPackagesAsync(CliInvocation invocation)
{
    // "packages" works in both published layout (packages/ next to the exe) and dev
    // layout (packages under the repo root).
    var defaultPackageRoot = Directory.Exists(ResolveWorkingPath("packages"))
        ? "packages"
        : "packages";
    var packageRoot = ResolveWorkingPath(
        invocation.Options.TryGetValue("root", out var root) ? root : defaultPackageRoot);

    if (!Directory.Exists(packageRoot))
    {
        Console.WriteLine($"Package root not found: {packageRoot}");
        return Task.CompletedTask;
    }

    foreach (var directory in Directory.EnumerateDirectories(packageRoot).OrderBy(path => path, StringComparer.OrdinalIgnoreCase))
    {
        // A package is either a graph package (pipeline.json) or a legacy
        // dataset-capture package (manifest.json + profile.json).
        var hasPipeline = File.Exists(Path.Combine(directory, "pipeline.json"));
        var hasLegacyProfile = File.Exists(Path.Combine(directory, "manifest.json"))
            && File.Exists(Path.Combine(directory, "profile.json"));
        if (!hasPipeline && !hasLegacyProfile)
        {
            continue;
        }

        var kind = hasPipeline ? "graph" : "legacy";
        Console.WriteLine($"{Path.GetFileName(directory)} | {kind} | {directory}");
    }

    return Task.CompletedTask;
}

Task ListSessionsAsync(CliInvocation invocation)
{
    var sessionRoot = ResolveWorkingPath(
        invocation.Options.TryGetValue("root", out var root) ? root : "artifacts/datasets");

    if (!Directory.Exists(sessionRoot))
    {
        Console.WriteLine($"Session root not found: {sessionRoot}");
        return Task.CompletedTask;
    }

    foreach (var directory in Directory.EnumerateDirectories(sessionRoot).OrderByDescending(path => path, StringComparer.OrdinalIgnoreCase))
    {
        var sessionMetadataPath = Path.Combine(directory, "session.json");
        if (!File.Exists(sessionMetadataPath))
        {
            continue;
        }

        Console.WriteLine($"{Path.GetFileName(directory)} | {directory}");
    }

    return Task.CompletedTask;
}

async Task InspectSessionAsync(CliInvocation invocation)
{
    if (!invocation.Options.TryGetValue("path", out var path))
    {
        Console.WriteLine("Missing required option: --path <session-folder-or-session.json>");
        return;
    }

    var resolvedPath = ResolveWorkingPath(path);
    var sessionMetadataPath = Directory.Exists(resolvedPath)
        ? Path.Combine(resolvedPath, "session.json")
        : resolvedPath;

    if (!File.Exists(sessionMetadataPath))
    {
        Console.WriteLine($"Session metadata not found: {sessionMetadataPath}");
        return;
    }

    var session = await ReadJsonAsync<DatasetSessionSummary>(sessionMetadataPath);

    Console.WriteLine($"SessionRoot: {session.SessionRoot}");
    Console.WriteLine($"FrameCount: {session.FrameCount}");
    Console.WriteLine($"FinalizedAtUtc: {session.FinalizedAtUtc:O}");
    Console.WriteLine($"Records: {session.Records.Count}");

    foreach (var cameraGroup in session.Records.GroupBy(record => record.CameraId).OrderBy(group => group.Key, StringComparer.OrdinalIgnoreCase))
    {
        Console.WriteLine($"  {cameraGroup.Key}: {cameraGroup.Count()} frame(s)");
    }
}

Task ListModulesAsync(CliInvocation invocation)
{
    using var host = BuildHost();
    var options = host.Services.GetRequiredService<Microsoft.Extensions.Options.IOptions<MachineVisionFabricRuntimeOptions>>().Value;
    var integrationsRoot = ResolveWorkingPath(
        invocation.Options.TryGetValue("root", out var root) ? root : options.IntegrationsRoot);
    var loader = host.Services.GetRequiredService<IIntegrationModuleLoader>();

    foreach (var module in loader.LoadModules(integrationsRoot).OrderBy(module => module.Describe().ModuleId, StringComparer.OrdinalIgnoreCase))
    {
        var descriptor = module.Describe();
        var capabilities = string.Join(", ", descriptor.Capabilities.Select(capability => $"{capability.Kind}:{capability.Name}"));
        Console.WriteLine($"{descriptor.ModuleId} | {descriptor.DisplayName} | {descriptor.Version} | {capabilities}");
    }

    return Task.CompletedTask;
}

Task ExportSchemasAsync(CliInvocation invocation)
{
    var outputRoot = ResolveWorkingPath(
        invocation.Options.TryGetValue("output", out var output) ? output : "artifacts/schemas");

    Directory.CreateDirectory(outputRoot);

    var jsonOptions = new JsonSerializerOptions(JsonSerializerDefaults.Web)
    {
        WriteIndented = true,
        TypeInfoResolver = new DefaultJsonTypeInfoResolver(),
        Converters = { new JsonStringEnumConverter() }
    };

    WriteSchema<IntegrationModuleDescriptor>("integration-module-descriptor.schema.json");
    WriteSchema<ModuleManifest>("module-manifest.schema.json");
    WriteSchema<IntegrationCapabilityDescriptor>("integration-capability-descriptor.schema.json");
    WriteSchema<ModulePortDescriptor>("module-port-descriptor.schema.json");
    WriteSchema<PipelineDefinition>("pipeline-definition.schema.json");
    WriteSchema<PipelineNodeDefinition>("pipeline-node-definition.schema.json");
    WriteSchema<PipelineEdgeDefinition>("pipeline-edge-definition.schema.json");
    WriteSchema<PipelinePortDefinition>("pipeline-port-definition.schema.json");
    WriteSchema<PipelineValidationResult>("pipeline-validation-result.schema.json");
    WriteSchema<S7GatewayGateOptions>("s7-gateway-gate-options.schema.json");
    WriteSchema<S7SignalAddress>("s7-signal-address.schema.json");
    WriteSchema<SimulatedPlcGateOptions>("simulated-plc-gate-options.schema.json");
    WriteSchema<TcpSignalGateOptions>("tcp-signal-gate-options.schema.json");
    WriteSchema<FolderSequenceSourceOptions>("folder-sequence-source-options.schema.json");

    Console.WriteLine($"SchemaOutput: {outputRoot}");
    return Task.CompletedTask;

    void WriteSchema<T>(string fileName)
    {
        JsonNode schema = JsonSchemaExporter.GetJsonSchemaAsNode(jsonOptions, typeof(T), exporterOptions: null);
        File.WriteAllText(Path.Combine(outputRoot, fileName), schema.ToJsonString(jsonOptions));
    }
}

async Task ValidatePipelineAsync(CliInvocation invocation)
{
    if (!invocation.Options.TryGetValue("path", out var path))
    {
        Console.WriteLine("Missing required option: --path <pipeline.json>");
        Environment.ExitCode = 1;
        return;
    }

    var pipelinePath = ResolveWorkingPath(path);
    if (!File.Exists(pipelinePath))
    {
        Console.WriteLine($"Pipeline definition not found: {pipelinePath}");
        Environment.ExitCode = 1;
        return;
    }

    using var host = BuildHost();
    var runtimeOptions = host.Services.GetRequiredService<Microsoft.Extensions.Options.IOptions<MachineVisionFabricRuntimeOptions>>().Value;
    var integrationsRoot = ResolveWorkingPath(
        invocation.Options.TryGetValue("integrations-root", out var intRoot) ? intRoot : runtimeOptions.IntegrationsRoot);

    var definition = await LoadPipelineAsync(pipelinePath, integrationsRoot, host);
    if (definition is null)
    {
        return;
    }

    var validator = host.Services.GetRequiredService<IPipelineDefinitionValidator>();
    var result = validator.Validate(definition);

    Console.WriteLine($"Pipeline: {definition.Name}");
    Console.WriteLine($"Nodes: {definition.Nodes.Count}");
    Console.WriteLine($"Edges: {definition.Edges.Count}");
    Console.WriteLine($"IsValid: {result.IsValid}");

    foreach (var issue in result.Issues)
    {
        Console.WriteLine($"{issue.Severity.ToUpperInvariant()} {issue.Code} | node={issue.NodeId ?? "-"} | edge={issue.EdgeId ?? "-"} | {issue.Message}");
    }

    if (!result.IsValid)
    {
        Environment.ExitCode = 1;
    }
}

async Task ExecuteGraphAsync(CliInvocation invocation)
{
    using var host = BuildHost();
    var runtimeOptions = host.Services.GetRequiredService<Microsoft.Extensions.Options.IOptions<MachineVisionFabricRuntimeOptions>>().Value;
    var integrationsRoot = ResolveWorkingPath(
        invocation.Options.TryGetValue("integrations-root", out var intRoot) ? intRoot : runtimeOptions.IntegrationsRoot);

    var packageRoot = invocation.Options.TryGetValue("package", out var pkg)
        ? ResolveWorkingPath(pkg)
        : ResolveWorkingPath(runtimeOptions.DatasetCapture.PackageRoot);

    // Resolve pipeline.json: explicit --path wins, then package pipeline.json
    string pipelinePath;
    if (invocation.Options.TryGetValue("path", out var path))
    {
        pipelinePath = ResolveWorkingPath(path);
    }
    else
    {
        pipelinePath = Path.Combine(packageRoot, "pipeline.json");
    }

    if (!File.Exists(pipelinePath))
    {
        Console.Error.WriteLine($"Pipeline definition not found: {pipelinePath}");
        Environment.ExitCode = 1;
        return;
    }

    var definition = await LoadPipelineAsync(pipelinePath, integrationsRoot, host);
    if (definition is null)
    {
        return;
    }

    var maxCycles = invocation.Options.TryGetValue("max-cycles", out var mc) && int.TryParse(mc, out var mcInt)
        ? mcInt
        : 0;

    var checkpointEvery = invocation.Options.TryGetValue("checkpoint-every", out var ce) && int.TryParse(ce, out var ceInt)
        ? ceInt
        : 0;

    // Durable resume: when checkpointing is on, persist under the package (or --resume-dir). An
    // interrupted run reloads this on the next start; a clean run clears it.
    string? checkpointDirectory = null;
    if (checkpointEvery > 0)
    {
        checkpointDirectory = invocation.Options.TryGetValue("resume-dir", out var rd)
            ? ResolveWorkingPath(rd)
            : Path.Combine(packageRoot, ".mvf", "checkpoint");
    }

    // Backpressure policy for the shared data plane: stall (lossless, default) fails a run rather than
    // lose a frame; drop (lossy) skips a frame for out-of-process consumers when the arena is full.
    var backpressure = invocation.Options.TryGetValue("backpressure", out var bp)
        && string.Equals(bp, "drop", StringComparison.OrdinalIgnoreCase)
        ? BackpressurePolicy.Drop
        : BackpressurePolicy.Stall;

    var options = new PipelineExecutionOptions
    {
        PackageRoot = packageRoot,
        IntegrationsRoot = integrationsRoot,
        MaxCycles = maxCycles,
        CheckpointIntervalCycles = checkpointEvery,
        CheckpointDirectory = checkpointDirectory,
        BackpressurePolicy = backpressure
    };

    var validator = host.Services.GetRequiredService<IPipelineDefinitionValidator>();
    var validation = validator.Validate(definition);

    if (!validation.IsValid)
    {
        AnsiConsole.MarkupLine($"[bold]{Markup.Escape(definition.Name)}[/]  [red]✖ Invalid pipeline[/]");
        foreach (var issue in validation.Issues)
        {
            AnsiConsole.MarkupLine($"  [red]{Markup.Escape(issue.Severity.ToUpperInvariant())}[/] {issue.Code} | {Markup.Escape(issue.Message)}");
        }
        Environment.ExitCode = 1;
        return;
    }

    var noTui = invocation.Options.ContainsKey("no-tui") || !AnsiConsole.Profile.Capabilities.Ansi;

    if (noTui)
    {
        // Plain text fallback
        var executionHost = host.Services.GetRequiredService<IPipelineExecutionHost>();
        await using var _ = executionHost;
        Console.WriteLine($"Pipeline: {definition.Name}  nodes:{definition.Nodes.Count}  edges:{definition.Edges.Count}");
        await executionHost.StartAsync(definition, options);
        var report = await executionHost.WaitForCompletionAsync();
        PrintExecutionReport(report);
        if (report is null || !report.Succeeded)
        {
            Console.Error.WriteLine($"Error: {report?.ErrorMessage ?? "unknown"}");
            Environment.ExitCode = 1;
        }
        return;
    }

    // TUI dashboard
    var tuiHost = host.Services.GetRequiredService<IPipelineExecutionHost>();
    await using var _2 = tuiHost;
    var dashboard = new PipelineDashboard(tuiHost, definition);
    var dashReport = await dashboard.RunAsync(options);

    // The dashboard is transient (it repaints in place); print the summary so the run leaves a record
    // in the scrollback — worker restarts and RPC latency included.
    Console.WriteLine();
    PrintExecutionReport(dashReport);

    if (dashReport is null || !dashReport.Succeeded)
    {
        Environment.ExitCode = 1;
    }
}

// Post-run summary shared by the plain and TUI paths: run totals, backpressure, and per-node stats.
// Worker-backed nodes get a second line of cross-process health — a supervised restart is invisible to
// the graph (the retry just succeeds), so this is the only place it is recorded.
void PrintExecutionReport(PipelineExecutionReport? report)
{
    Console.WriteLine(
        $"Succeeded:{report?.Succeeded}  cycles:{report?.TotalCycles}  accepted:{report?.AcceptedCycles}  " +
        $"dropped:{report?.DroppedFrames}  restarts:{report?.WorkerRestarts}  duration:{report?.Duration.TotalSeconds:F2}s");

    if (report is { DroppedFrames: > 0 })
    {
        Console.WriteLine($"  backpressure: dropped {report.DroppedFrames} frame(s) (policy=drop, arena full)");
    }

    foreach (var (nid, ns) in report?.NodeStats ?? new Dictionary<string, NodeExecutionStats>())
    {
        Console.WriteLine(
            $"  {nid}: mode={ns.ActivationMode} warmup={ns.WarmupMs}ms cycles={ns.TotalCycles} " +
            $"faults={ns.FaultedCycles} avg={ns.AverageDurationMs:F1}ms");

        if (ns.Worker is not { } w)
        {
            continue;
        }

        // warm=x/y — how many restarts were served by a pre-warmed spare instead of a cold start (L.4).
        var warm = w.Restarts > 0 ? $" warm={w.WarmRestarts}/{w.Restarts}" : string.Empty;
        Console.WriteLine(
            $"      worker={w.ModuleId} rpc={w.Requests} failed={w.FailedRequests} restarts={w.Restarts}{warm} " +
            $"avg={w.AverageRequestMs:F1}ms max={w.MaxRequestMs:F1}ms");

        if (w.LastRestartUtc is { } at)
        {
            Console.WriteLine($"      last restart {at:HH:mm:ss} UTC — {w.LastRestartReason}");
        }
    }
}

// Reads a pipeline.json (lean or rich) and expands it into the rich model the validator and
// executor consume. Lean authoring lives entirely in the expander; both commands share this.
async Task<PipelineDefinition?> LoadPipelineAsync(string pipelinePath, string integrationsRoot, IHost host)
{
    var json = await File.ReadAllTextAsync(pipelinePath);
    var catalog = host.Services.GetRequiredService<ModuleCatalog>().Load(integrationsRoot);
    var expander = host.Services.GetRequiredService<PipelineExpander>();

    try
    {
        return expander.Expand(json, catalog);
    }
    catch (PipelineExpansionException ex)
    {
        Console.Error.WriteLine($"Pipeline could not be expanded: {ex.Message}");
        Environment.ExitCode = 1;
        return null;
    }
}

IHost BuildHost(IReadOnlyDictionary<string, string?>? overrides = null)
{
    var builder = Host.CreateApplicationBuilder([]);
    builder.Configuration.Sources.Clear();
    builder.Configuration
        .SetBasePath(AppContext.BaseDirectory)
        .AddJsonFile("appsettings.json", optional: false, reloadOnChange: false)
        .AddEnvironmentVariables();

    if (overrides is not null && overrides.Count > 0)
    {
        builder.Configuration.AddInMemoryCollection(overrides);
    }

    builder.Services.Configure<MachineVisionFabricRuntimeOptions>(
        builder.Configuration.GetSection(MachineVisionFabricRuntimeOptions.SectionName));
    builder.Services.AddSingleton<ISimulatorSourceCatalog, EmptySimulatorSourceCatalog>();
    builder.Services.AddSingleton<IIntegrationModuleLoader, IntegrationModuleLoader>();
    builder.Services.AddSingleton<IPipelineDefinitionValidator, PipelineDefinitionValidator>();
    builder.Services.AddSingleton<ModuleCatalog>();
    builder.Services.AddSingleton<PipelineExpander>();
    // One engine-owned data plane per run, behind the IDataPlane seam. The backing file is created
    // lazily, so commands with no out-of-process modules pay nothing.
    builder.Services.AddSingleton<IDataPlane>(_ => new SharedMemoryArena(new SharedMemoryArenaOptions()));
    builder.Services.AddSingleton<IOutOfProcessModuleHost, Mvf.Hosting.Worker.StdioModuleHost>();
    builder.Services.AddSingleton<IPipelineNodeActivator, PipelineNodeActivator>();
    builder.Services.AddSingleton<IPipelineGraphExecutor, PipelineGraphExecutor>();
    builder.Services.AddTransient<IPipelineExecutionHost, PipelineExecutionHost>();

    return builder.Build();
}

string ResolveWorkingPath(string path)
{
    var repositoryRoot = ResolveRepositoryRoot();

    return Path.IsPathRooted(path)
        ? Path.GetFullPath(path)
        : Path.GetFullPath(Path.Combine(repositoryRoot, path));
}

string ResolveRepositoryRoot()
{
    var baseDir = AppContext.BaseDirectory;

    // Published layout: integrations/ or packages/ sit next to the executable.
    // Check this first so a publish output inside the repo still resolves correctly.
    if (Directory.Exists(Path.Combine(baseDir, "integrations")) ||
        Directory.Exists(Path.Combine(baseDir, "packages")) ||
        Directory.Exists(Path.Combine(baseDir, "modules")))
    {
        return baseDir;
    }

    // Dev layout: walk up looking for a stable repository marker. Search from the
    // current working directory first (where the user invoked the CLI), then from
    // the executable location. Keying off .git / CLAUDE.md keeps this working even
    // as source folders are added or removed.
    var root = FindRepositoryRoot(Environment.CurrentDirectory)
        ?? FindRepositoryRoot(baseDir);
    if (root is not null)
    {
        return root;
    }

    // Last resort: resolve relative paths against the user's working directory
    // rather than the executable's bin folder, which is almost never what they mean.
    return Environment.CurrentDirectory;
}

static string? FindRepositoryRoot(string startDirectory)
{
    var current = new DirectoryInfo(startDirectory);
    while (current is not null)
    {
        var hasSrc = Directory.Exists(Path.Combine(current.FullName, "src"));
        var hasGit = Directory.Exists(Path.Combine(current.FullName, ".git"));
        var hasClaudeMd = File.Exists(Path.Combine(current.FullName, "CLAUDE.md"));
        if (hasSrc && (hasGit || hasClaudeMd))
        {
            return current.FullName;
        }

        current = current.Parent;
    }

    return null;
}

void PrintHelp()
{
    Console.WriteLine("Mvf.Cli");
    Console.WriteLine("Commands:");
    Console.WriteLine("  execute-graph [--path <pipeline.json>] [--package <path>] [--integrations-root <path>] [--max-cycles <n>] [--checkpoint-every <n>] [--resume-dir <path>] [--backpressure stall|drop] [--no-tui]");
    Console.WriteLine("  validate-pipeline --path <pipeline.json> [--integrations-root <path>]");
    Console.WriteLine("  modules [--root <path>]");
    Console.WriteLine("  packages [--root <path>]");
    Console.WriteLine("  sessions [--root <path>]");
    Console.WriteLine("  inspect-session --path <session-folder-or-session.json>");
    Console.WriteLine("  schemas [--output <path>]");
}

async Task<T> ReadJsonAsync<T>(string path)
{
    await using var stream = File.OpenRead(path);
    var value = await JsonSerializer.DeserializeAsync<T>(stream, jsonOptions, CancellationToken.None);

    return value ?? throw new InvalidOperationException($"Could not deserialize JSON file '{path}'.");
}

internal sealed class CliInvocation
{
    public required string Command { get; init; }

    public required bool ShowHelp { get; init; }

    public required IReadOnlyDictionary<string, string> Options { get; init; }

    public static CliInvocation Parse(string[] args)
    {
        if (args.Length == 0)
        {
            return new CliInvocation
            {
                Command = "help",
                ShowHelp = true,
                Options = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)
            };
        }

        var first = args[0];
        if (first is "--help" or "-h" or "help")
        {
            return new CliInvocation
            {
                Command = "help",
                ShowHelp = true,
                Options = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)
            };
        }

        var options = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        for (var i = 1; i < args.Length; i++)
        {
            var token = args[i];
            if (!token.StartsWith("--", StringComparison.Ordinal))
            {
                continue;
            }

            var key = token[2..];
            var value = i + 1 < args.Length && !args[i + 1].StartsWith("--", StringComparison.Ordinal)
                ? args[++i]
                : "true";

            options[key] = value;
        }

        return new CliInvocation
        {
            Command = first,
            ShowHelp = false,
            Options = options
        };
    }
}
