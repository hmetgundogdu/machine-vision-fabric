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

        var modules = new List<IIntegrationModule>();
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

            Assembly assembly;
            try
            {
                var loadContext = new IntegrationPluginLoadContext(assemblyPath);
                assembly = loadContext.LoadFromAssemblyPath(assemblyPath);
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
                string.Equals(type.FullName, manifest.EntryType, StringComparison.Ordinal));

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
            if (!string.Equals(descriptor.ModuleId, manifest.ModuleId, StringComparison.OrdinalIgnoreCase))
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

        if (!normalizedPath.Contains($"{Path.DirectorySeparatorChar}bin{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }

        return !normalizedPath.Contains($"{Path.DirectorySeparatorChar}ref{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase)
            && !normalizedPath.Contains($"{Path.DirectorySeparatorChar}refint{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase)
            && !normalizedPath.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase);
    }

    private static bool IsRuntimeAssemblyPath(string assemblyPath)
    {
        var normalizedPath = Path.GetFullPath(assemblyPath);

        if (!normalizedPath.Contains($"{Path.DirectorySeparatorChar}bin{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }

        return !normalizedPath.Contains($"{Path.DirectorySeparatorChar}ref{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase)
            && !normalizedPath.Contains($"{Path.DirectorySeparatorChar}refint{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase)
            && !normalizedPath.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase);
    }
}
