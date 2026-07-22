using System.Reflection;
using System.Text.Json;
using System.Text.Json.Serialization;
using Mvf.Graph.Integrations;
using Mvf.Abstractions;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Logging.Abstractions;

namespace Mvf.Engine.Plugins;

/// <summary>
/// Discovers .NET modules by scanning <c>module.json</c> manifests under a plugin root and
/// loading the assembly beside each one. The entry type is auto-discovered (the single public
/// <see cref="IIntegrationModule"/> in the assembly) — the manifest never names it.
///
/// Non-.NET (process) modules also use <c>module.json</c> but are handled by the worker host,
/// not here; this loader skips them.
///
/// A module that cannot be loaded is skipped rather than failing the whole scan, so one broken
/// plugin never takes the runtime down; every skip is logged with its reason.
/// </summary>
public sealed class IntegrationModuleLoader(ILogger<IntegrationModuleLoader>? logger = null) : IIntegrationModuleLoader
{
    private readonly ILogger _logger = logger ?? NullLogger<IntegrationModuleLoader>.Instance;

    private const string ManifestFileName = "module.json";

    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
    {
        Converters = { new JsonStringEnumConverter() }
    };
    private static readonly string[] IgnoredAssemblyNames =
    [
        "Mvf.Graph",
        "Mvf.Abstractions"
    ];

    public IReadOnlyList<IIntegrationModule> LoadModules(string pluginRoot)
    {
        var fullPluginRoot = Path.GetFullPath(pluginRoot);
        if (!Directory.Exists(fullPluginRoot))
        {
            return [];
        }

        var candidatesByModuleId = new Dictionary<string, ModuleCandidate>(StringComparer.OrdinalIgnoreCase);
        var missingAssemblyByModuleId = new Dictionary<string, (string Entry, string AssemblyPath)>(StringComparer.OrdinalIgnoreCase);
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

            // Process (python/node/...) modules use the same manifest but are launched by the
            // worker host, not loaded as .NET assemblies here.
            if (!string.Equals(manifest.Runtime, "dotnet", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            var assemblyPath = Path.GetFullPath(Path.Combine(Path.GetDirectoryName(manifestPath) ?? fullPluginRoot, manifest.Entry));
            if (!File.Exists(assemblyPath) || !IsRuntimeAssemblyPath(assemblyPath))
            {
                // Defer this warning. A module commonly appears twice under one root: a
                // source-tree manifest with no co-located DLL, plus a built copy under bin/
                // that loads fine. Only warn below if no deployable copy is found anywhere.
                missingAssemblyByModuleId.TryAdd(manifest.Id, (manifest.Entry, assemblyPath));
                continue;
            }

            var assemblyFileName = Path.GetFileNameWithoutExtension(assemblyPath);
            if (IgnoredAssemblyNames.Contains(assemblyFileName, StringComparer.OrdinalIgnoreCase))
            {
                continue;
            }

            var candidate = new ModuleCandidate(manifest, manifestPath, assemblyPath);
            if (!candidatesByModuleId.TryGetValue(manifest.Id, out var existing)
                || CompareCandidates(candidate, existing, fullPluginRoot) < 0)
            {
                candidatesByModuleId[manifest.Id] = candidate;
            }
        }

        foreach (var (moduleId, info) in missingAssemblyByModuleId)
        {
            if (candidatesByModuleId.ContainsKey(moduleId))
            {
                continue;
            }

            _logger.LogWarning(
                "Module '{ModuleId}' skipped: entry assembly '{Entry}' was not found at '{AssemblyPath}'.",
                moduleId, info.Entry, info.AssemblyPath);
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
                    "Module '{ModuleId}' skipped: '{AssemblyPath}' is not a loadable .NET assembly: {Reason}",
                    candidate.Manifest.Id, candidate.AssemblyPath, ex.Message);
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
                    "Module '{ModuleId}' skipped: its types could not be loaded, which usually means a missing dependency: {Reason}",
                    candidate.Manifest.Id, loaderReasons);
                continue;
            }

            var entryTypes = exportedTypes
                .Where(type => !type.IsAbstract && !type.IsInterface && typeof(IIntegrationModule).IsAssignableFrom(type))
                .ToArray();

            if (entryTypes.Length == 0)
            {
                _logger.LogWarning(
                    "Module '{ModuleId}' skipped: no public IIntegrationModule type found in '{AssemblyPath}'.",
                    candidate.Manifest.Id, candidate.AssemblyPath);
                continue;
            }

            if (entryTypes.Length > 1)
            {
                _logger.LogWarning(
                    "Module '{ModuleId}' skipped: assembly '{AssemblyPath}' has {Count} IIntegrationModule types; exactly one is required.",
                    candidate.Manifest.Id, candidate.AssemblyPath, entryTypes.Length);
                continue;
            }

            if (Activator.CreateInstance(entryTypes[0]) is not IIntegrationModule module)
            {
                _logger.LogWarning(
                    "Module '{ModuleId}' skipped: entry type '{EntryType}' could not be instantiated.",
                    candidate.Manifest.Id, entryTypes[0].FullName);
                continue;
            }

            var descriptor = module.Describe();
            if (!string.Equals(descriptor.ModuleId, candidate.Manifest.Id, StringComparison.OrdinalIgnoreCase))
            {
                _logger.LogWarning(
                    "Module skipped: manifest '{ManifestPath}' declares id '{ManifestId}' but the code reports '{DescribedId}'.",
                    candidate.ManifestPath, candidate.Manifest.Id, descriptor.ModuleId);
                continue;
            }

            modules.Add(module);
        }

        return modules;
    }

    private ModuleManifest? LoadManifest(string manifestPath)
    {
        try
        {
            var json = File.ReadAllText(manifestPath);
            var manifest = JsonSerializer.Deserialize<ModuleManifest>(json, JsonOptions);
            if (manifest is null)
            {
                _logger.LogWarning("Module manifest '{ManifestPath}' is empty.", manifestPath);
            }

            return manifest;
        }
        catch (IOException ex)
        {
            _logger.LogWarning(ex, "Module manifest '{ManifestPath}' could not be read.", manifestPath);
            return null;
        }
        catch (UnauthorizedAccessException ex)
        {
            _logger.LogWarning(ex, "Module manifest '{ManifestPath}' could not be read.", manifestPath);
            return null;
        }
        catch (JsonException ex)
        {
            _logger.LogWarning(
                "Module manifest '{ManifestPath}' is invalid and its module will not be available: {Reason}",
                manifestPath, ex.Message);
            return null;
        }
    }

    private static bool IsRuntimeManifestPath(string manifestPath)
    {
        var normalizedPath = Path.GetFullPath(manifestPath);

        // Exclude build-time artifact directories (obj, ref/refint).
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
        ModuleManifest Manifest,
        string ManifestPath,
        string AssemblyPath);
}
