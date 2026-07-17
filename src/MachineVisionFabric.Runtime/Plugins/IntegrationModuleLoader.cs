using System.Reflection;
using System.Text.Json;
using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Plugins;

public sealed class IntegrationModuleLoader : IIntegrationModuleLoader
{
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
            catch (BadImageFormatException)
            {
                continue;
            }

            Type[] exportedTypes;
            try
            {
                exportedTypes = assembly.GetExportedTypes();
            }
            catch (ReflectionTypeLoadException)
            {
                continue;
            }

            var exportedType = exportedTypes.FirstOrDefault(type =>
                string.Equals(type.FullName, candidate.Manifest.EntryType, StringComparison.Ordinal));

            if (exportedType is null || exportedType.IsAbstract || exportedType.IsInterface)
            {
                continue;
            }

            if (!typeof(IIntegrationModule).IsAssignableFrom(exportedType))
            {
                continue;
            }

            if (Activator.CreateInstance(exportedType) is not IIntegrationModule module)
            {
                continue;
            }

            var descriptor = module.Describe();
            if (!string.Equals(descriptor.ModuleId, candidate.Manifest.ModuleId, StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            modules.Add(module);
        }

        return modules;
    }

    private static IntegrationModuleManifest? LoadManifest(string manifestPath)
    {
        try
        {
            var json = File.ReadAllText(manifestPath);
            return JsonSerializer.Deserialize<IntegrationModuleManifest>(json, JsonOptions);
        }
        catch (IOException)
        {
            return null;
        }
        catch (UnauthorizedAccessException)
        {
            return null;
        }
        catch (JsonException)
        {
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
