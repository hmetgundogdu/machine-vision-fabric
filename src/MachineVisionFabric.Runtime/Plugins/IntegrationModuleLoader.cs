using System.Reflection;
using System.Text.Json;
using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Logging.Abstractions;

namespace MachineVisionFabric.Runtime.Plugins;

/// <summary>
/// Discovers integration modules by scanning <c>integration-module.json</c> manifests under a plugin root.
///
/// A module that cannot be loaded is skipped rather than failing the whole scan, so one broken
/// plugin never takes the runtime down. Every skip is logged as a warning with its reason —
/// without that, a rejected module is indistinguishable from one that was never deployed.
/// </summary>
public sealed class IntegrationModuleLoader(ILogger<IntegrationModuleLoader>? logger = null) : IIntegrationModuleLoader
{
    private readonly ILogger _logger = logger ?? NullLogger<IntegrationModuleLoader>.Instance;

    private const string ManifestFileName = "integration-module.json";
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private static readonly string[] IgnoredAssemblyNames =
    [
        "MachineVisionFabric.Contracts",
        "MachineVisionFabric.Core"
    ];

    public IReadOnlyList<IIntegrationModule> LoadModules(string pluginRoot)
    {
        var fullPluginRoot = Path.GetFullPath(pluginRoot);
        if (!Directory.Exists(fullPluginRoot))
        {
            return [];
        }

        var candidatesByModuleId = new Dictionary<string, ModuleCandidate>(StringComparer.OrdinalIgnoreCase);
        foreach (var manifestPath in Directory.EnumerateFiles(fullPluginRoot, ManifestFileName, SearchOption.AllDirectories))
        {
            if (!IsRuntimeManifestPath(manifestPath))
            {
                continue;
            }

            var manifest = LoadManifest(manifestPath);
            if (manifest is null)
            {
                continue;
            }

            var assemblyPath = Path.GetFullPath(Path.Combine(Path.GetDirectoryName(manifestPath) ?? fullPluginRoot, manifest.EntryAssembly));
            if (!File.Exists(assemblyPath) || !IsRuntimeAssemblyPath(assemblyPath))
            {
                _logger.LogWarning(
                    "Integration module '{ModuleId}' skipped: entry assembly '{EntryAssembly}' was not found at '{AssemblyPath}'.",
                    manifest.ModuleId, manifest.EntryAssembly, assemblyPath);
                continue;
            }

            var assemblyFileName = Path.GetFileNameWithoutExtension(assemblyPath);
            if (IgnoredAssemblyNames.Contains(assemblyFileName, StringComparer.OrdinalIgnoreCase))
            {
                continue;
            }

            var candidate = new ModuleCandidate(manifest, manifestPath, assemblyPath);
            if (!candidatesByModuleId.TryGetValue(manifest.ModuleId, out var existing)
                || CompareCandidates(candidate, existing, fullPluginRoot) < 0)
            {
                candidatesByModuleId[manifest.ModuleId] = candidate;
            }
        }

        var modules = new List<IIntegrationModule>();
        foreach (var candidate in candidatesByModuleId.Values)
        {
            Assembly assembly;
            try
            {
                var loadContext = new IntegrationPluginLoadContext(candidate.AssemblyPath);
                assembly = loadContext.LoadFromAssemblyPath(candidate.AssemblyPath);
            }
            catch (BadImageFormatException ex)
            {
                _logger.LogWarning(
                    "Integration module '{ModuleId}' skipped: '{AssemblyPath}' is not a loadable .NET assembly: {Reason}",
                    candidate.Manifest.ModuleId, candidate.AssemblyPath, ex.Message);
                continue;
            }

            Type[] exportedTypes;
            try
            {
                exportedTypes = assembly.GetExportedTypes();
            }
            catch (ReflectionTypeLoadException ex)
            {
                var loaderReasons = string.Join("; ", ex.LoaderExceptions.Select(loaderException => loaderException?.Message));
                _logger.LogWarning(
                    "Integration module '{ModuleId}' skipped: its types could not be loaded, which usually means a missing dependency: {Reason}",
                    candidate.Manifest.ModuleId, loaderReasons);
                continue;
            }

            var exportedType = exportedTypes.FirstOrDefault(type =>
                string.Equals(type.FullName, candidate.Manifest.EntryType, StringComparison.Ordinal));

            if (exportedType is null || exportedType.IsAbstract || exportedType.IsInterface)
            {
                _logger.LogWarning(
                    "Integration module '{ModuleId}' skipped: entry type '{EntryType}' is missing, abstract, or not public in '{AssemblyPath}'.",
                    candidate.Manifest.ModuleId, candidate.Manifest.EntryType, candidate.AssemblyPath);
                continue;
            }

            if (!typeof(IIntegrationModule).IsAssignableFrom(exportedType))
            {
                _logger.LogWarning(
                    "Integration module '{ModuleId}' skipped: entry type '{EntryType}' does not implement IIntegrationModule.",
                    candidate.Manifest.ModuleId, candidate.Manifest.EntryType);
                continue;
            }

            if (Activator.CreateInstance(exportedType) is not IIntegrationModule module)
            {
                _logger.LogWarning(
                    "Integration module '{ModuleId}' skipped: entry type '{EntryType}' could not be instantiated.",
                    candidate.Manifest.ModuleId, candidate.Manifest.EntryType);
                continue;
            }

            var descriptor = module.Describe();
            if (!string.Equals(descriptor.ModuleId, candidate.Manifest.ModuleId, StringComparison.OrdinalIgnoreCase))
            {
                _logger.LogWarning(
                    "Integration module skipped: manifest '{ManifestPath}' declares module id '{ManifestModuleId}' but the code reports '{DescribedModuleId}'.",
                    candidate.ManifestPath, candidate.Manifest.ModuleId, descriptor.ModuleId);
                continue;
            }

            modules.Add(module);
        }

        return modules;
    }

    private IntegrationModuleManifest? LoadManifest(string manifestPath)
    {
        try
        {
            var json = File.ReadAllText(manifestPath);
            var manifest = JsonSerializer.Deserialize<IntegrationModuleManifest>(json, JsonOptions);
            if (manifest is null)
            {
                _logger.LogWarning("Integration manifest '{ManifestPath}' is empty.", manifestPath);
            }

            return manifest;
        }
        catch (IOException ex)
        {
            _logger.LogWarning(ex, "Integration manifest '{ManifestPath}' could not be read.", manifestPath);
            return null;
        }
        catch (UnauthorizedAccessException ex)
        {
            _logger.LogWarning(ex, "Integration manifest '{ManifestPath}' could not be read.", manifestPath);
            return null;
        }
        catch (JsonException ex)
        {
            _logger.LogWarning(
                "Integration manifest '{ManifestPath}' is invalid and its module will not be available: {Reason}",
                manifestPath, ex.Message);
            return null;
        }
    }

    private static bool IsRuntimeManifestPath(string manifestPath)
    {
        var normalizedPath = Path.GetFullPath(manifestPath);

        // Exclude build-time artifact directories (source, obj, ref/refint)
        return !normalizedPath.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase)
            && !normalizedPath.Contains($"{Path.DirectorySeparatorChar}ref{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase)
            && !normalizedPath.Contains($"{Path.DirectorySeparatorChar}refint{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase);
    }

    private static bool IsRuntimeAssemblyPath(string assemblyPath)
    {
        var normalizedPath = Path.GetFullPath(assemblyPath);

        return !normalizedPath.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase)
            && !normalizedPath.Contains($"{Path.DirectorySeparatorChar}ref{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase)
            && !normalizedPath.Contains($"{Path.DirectorySeparatorChar}refint{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase);
    }

    private static int CompareCandidates(ModuleCandidate left, ModuleCandidate right, string pluginRoot)
    {
        var leftRelativeLength = Path.GetRelativePath(pluginRoot, left.ManifestPath).Length;
        var rightRelativeLength = Path.GetRelativePath(pluginRoot, right.ManifestPath).Length;

        var lengthComparison = leftRelativeLength.CompareTo(rightRelativeLength);
        if (lengthComparison != 0)
        {
            return lengthComparison;
        }

        return StringComparer.OrdinalIgnoreCase.Compare(left.ManifestPath, right.ManifestPath);
    }

    private sealed record ModuleCandidate(
        IntegrationModuleManifest Manifest,
        string ManifestPath,
        string AssemblyPath);
}
