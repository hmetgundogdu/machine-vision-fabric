using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Schema;
using System.Text.Json.Serialization.Metadata;
using MachineVisionFabric.Contracts.Control;
using MachineVisionFabric.Contracts.Dataset;
using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Contracts.Simulation;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Runtime;
using MachineVisionFabric.Runtime.Plugins;
using MachineVisionFabric.Sources.Simulators;
using MachineVisionFabric.Storage;
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
    case "inspect-package":
        await InspectPackageAsync(invocation);
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
    case "run":
        await RunAsync(invocation);
        break;
    default:
        Console.Error.WriteLine($"Unknown command '{invocation.Command}'.");
        PrintHelp();
        Environment.ExitCode = 1;
        break;
}

return;

async Task RunAsync(CliInvocation invocation)
{
    var overrides = new Dictionary<string, string?>(StringComparer.OrdinalIgnoreCase);

    if (invocation.Options.TryGetValue("integrations-root", out var integrationsRoot))
    {
        overrides["MachineVisionFabric:IntegrationsRoot"] = integrationsRoot;
    }

    if (invocation.Options.TryGetValue("package", out var packageRoot))
    {
        overrides["MachineVisionFabric:DatasetCapture:PackageRoot"] = packageRoot;
    }

    if (invocation.Options.TryGetValue("dataset-root", out var datasetRoot))
    {
        overrides["MachineVisionFabric:DatasetCapture:DatasetRoot"] = datasetRoot;
    }

    if (invocation.Options.TryGetValue("session-prefix", out var sessionPrefix))
    {
        overrides["MachineVisionFabric:DatasetCapture:SessionPrefix"] = sessionPrefix;
    }

    using var host = BuildHost(overrides);
    var bootstrapper = host.Services.GetRequiredService<IHeadlessRuntimeBootstrapper>();
    var report = await bootstrapper.BootstrapAsync(CancellationToken.None);

    Console.WriteLine($"PackageRoot: {report.PackageRoot}");
    Console.WriteLine($"SessionRoot: {report.DatasetSessionRoot}");
    Console.WriteLine($"FrameSource: {report.FrameSourceSource} ({report.FrameSourceStrategy})");
    Console.WriteLine($"ProductGate: {report.ProductPresenceSource} ({report.ProductPresenceStrategy})");
    Console.WriteLine($"ProductPresent: {report.ProductPresent}");
    Console.WriteLine($"CapturedFrames: {report.CapturedFrameCount}");
    Console.WriteLine($"SessionMetadata: {report.SessionMetadataPath}");
}

async Task InspectPackageAsync(CliInvocation invocation)
{
    var packagePath = invocation.Options.TryGetValue("package", out var packageRoot)
        ? ResolveWorkingPath(packageRoot)
        : ResolveWorkingPath("examples\\packages\\dataset-capture-starter");

    var manifestPath = Path.Combine(packagePath, "manifest.json");
    var profilePath = Path.Combine(packagePath, "profile.json");

    if (!File.Exists(manifestPath) || !File.Exists(profilePath))
    {
        Console.WriteLine($"Package files were not found under: {packagePath}");
        return;
    }

    var manifest = await ReadJsonAsync<FabricProfileManifest>(manifestPath);
    var profile = await ReadJsonAsync<FabricRuntimeProfile>(profilePath);

    Console.WriteLine($"Package: {manifest.Name}");
    Console.WriteLine($"Version: {manifest.Version}");
    Console.WriteLine($"Scenario: {manifest.Scenario}");
    Console.WriteLine($"EntryProfile: {manifest.EntryProfile}");
    Console.WriteLine($"ProfileMode: {profile.Mode}");
    Console.WriteLine($"Capabilities: {string.Join(", ", profile.Capabilities)}");
    Console.WriteLine(
        $"CapturePolicy: enabled={manifest.CapturePolicy.Enabled}; requireProductPresent={manifest.CapturePolicy.RequireProductPresent}; maxFramesPerCamera={manifest.CapturePolicy.MaxFramesPerCamera}; mode={manifest.CapturePolicy.Mode}; preTriggerFramesPerCamera={manifest.CapturePolicy.PreTriggerFramesPerCamera}; postTriggerFramesPerCamera={manifest.CapturePolicy.PostTriggerFramesPerCamera}; gateEvaluationIntervalFrames={manifest.CapturePolicy.GateEvaluationIntervalFrames}");
    Console.WriteLine($"ProductGate: mode={manifest.ProductPresenceGate.Mode}; moduleId={manifest.ProductPresenceGate.ModuleId ?? "-"}");
    Console.WriteLine($"FrameSource: mode={profile.Source.Mode}; moduleId={profile.Source.ModuleId ?? "-"}");
    Console.WriteLine($"RequiredDirectories: {string.Join(", ", manifest.RequiredDirectories)}");
}

Task ListPackagesAsync(CliInvocation invocation)
{
    var packageRoot = ResolveWorkingPath(
        invocation.Options.TryGetValue("root", out var root) ? root : "examples\\packages");

    if (!Directory.Exists(packageRoot))
    {
        Console.WriteLine($"Package root not found: {packageRoot}");
        return Task.CompletedTask;
    }

    foreach (var directory in Directory.EnumerateDirectories(packageRoot).OrderBy(path => path, StringComparer.OrdinalIgnoreCase))
    {
        var manifestPath = Path.Combine(directory, "manifest.json");
        var profilePath = Path.Combine(directory, "profile.json");
        if (!File.Exists(manifestPath) || !File.Exists(profilePath))
        {
            continue;
        }

        Console.WriteLine($"{Path.GetFileName(directory)} | {directory}");
    }

    return Task.CompletedTask;
}

Task ListSessionsAsync(CliInvocation invocation)
{
    var sessionRoot = ResolveWorkingPath(
        invocation.Options.TryGetValue("root", out var root) ? root : "artifacts\\datasets");

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

    var session = await ReadJsonAsync<DatasetSessionMetadata>(sessionMetadataPath);

    Console.WriteLine($"PackageName: {session.PackageName}");
    Console.WriteLine($"SessionRoot: {session.SessionRoot}");
    Console.WriteLine($"CreatedAtUtc: {session.CreatedAtUtc:O}");
    Console.WriteLine($"Scenario: {session.Scenario}");
    Console.WriteLine($"CapturedFrameCount: {session.CapturedFrameCount}");
    Console.WriteLine($"DeclaredCameraCount: {session.DeclaredCameraCount}");
    Console.WriteLine($"ProductPresent: {session.ProductPresenceDecision.ProductPresent}");
    Console.WriteLine($"ProductSource: {session.ProductPresenceDecision.Source}");
    Console.WriteLine($"Records: {session.Records.Count}");
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
        invocation.Options.TryGetValue("output", out var output) ? output : "artifacts\\schemas");

    Directory.CreateDirectory(outputRoot);

    var jsonOptions = new JsonSerializerOptions(JsonSerializerDefaults.Web)
    {
        WriteIndented = true,
        TypeInfoResolver = new DefaultJsonTypeInfoResolver()
    };

    WriteSchema<FabricProfileManifest>("fabric-profile-manifest.schema.json");
    WriteSchema<FabricRuntimeProfile>("fabric-runtime-profile.schema.json");
    WriteSchema<SourceBinding>("source-binding.schema.json");
    WriteSchema<IntegrationModuleDescriptor>("integration-module-descriptor.schema.json");
    WriteSchema<IntegrationModuleManifest>("integration-module-manifest.schema.json");
    WriteSchema<IntegrationCapabilityDescriptor>("integration-capability-descriptor.schema.json");
    WriteSchema<ProductPresenceGateBinding>("product-presence-gate-binding.schema.json");
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
    builder.Services.AddSingleton<IPackageManifestLoader, PackageManifestLoader>();
    builder.Services.AddSingleton<IEntryProfileLoader, EntryProfileLoader>();
    builder.Services.AddSingleton<IDatasetSessionPreparer, DatasetSessionPreparer>();
    builder.Services.AddSingleton<IDatasetCollector, DatasetCollector>();
    builder.Services.AddSingleton<ISimulatorSourceCatalog, FolderSequenceSourceCatalog>();
    builder.Services.AddSingleton<IIntegrationModuleLoader, IntegrationModuleLoader>();
    builder.Services.AddSingleton<IFrameSourceResolver, ProfileFrameSourceResolver>();
    builder.Services.AddSingleton<IProductPresenceGateResolver, ProfileProductPresenceGateResolver>();
    builder.Services.AddSingleton<IHeadlessRuntimeBootstrapper, HeadlessRuntimeBootstrapper>();

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
    var currentDirectory = new DirectoryInfo(AppContext.BaseDirectory);
    while (currentDirectory is not null)
    {
        var hasSrc = Directory.Exists(Path.Combine(currentDirectory.FullName, "src"));
        var hasExamples = Directory.Exists(Path.Combine(currentDirectory.FullName, "examples"));
        var hasLegacySamples = Directory.Exists(Path.Combine(currentDirectory.FullName, "samples"));
        if (hasSrc && (hasExamples || hasLegacySamples))
        {
            return currentDirectory.FullName;
        }

        currentDirectory = currentDirectory.Parent;
    }

    return AppContext.BaseDirectory;
}

void PrintHelp()
{
    Console.WriteLine("MachineVisionFabric.Cli");
    Console.WriteLine("Commands:");
    Console.WriteLine("  run [--integrations-root <path>] [--package <path>] [--dataset-root <path>] [--session-prefix <value>]");
    Console.WriteLine("  packages [--root <path>]");
    Console.WriteLine("  inspect-package [--package <path>]");
    Console.WriteLine("  modules [--root <path>]");
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
                Command = "run",
                ShowHelp = false,
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
