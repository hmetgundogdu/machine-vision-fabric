using System.Text.Json;
using System.Text.Json.Serialization;
using Mvf.Graph.Integrations;

namespace Mvf.Engine.Modules;

/// <summary>
/// Reads <c>module.json</c> manifests under a plugin root WITHOUT loading any assembly.
/// Lean pipeline expansion needs only each module's <see cref="ModuleManifest.Kind"/> and
/// <see cref="ModuleManifest.Runtime"/> to derive standard ports; it must not pay the cost
/// (or the failure risk) of loading .NET plugins. This is the read-only, metadata-only
/// sibling of <c>IntegrationModuleLoader</c>.
/// </summary>
public sealed class ModuleCatalog
{
    private const string ManifestFileName = "module.json";

    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
    {
        Converters = { new JsonStringEnumConverter() }
    };

    /// <summary>
    /// Scans <paramref name="pluginRoot"/> recursively and returns each module's manifest
    /// keyed by its id (case-insensitive). Build-artifact directories (obj/ref/refint) are
    /// skipped; when a module appears under both the source tree and a <c>bin/</c> copy, the
    /// shallower path wins so the result is deterministic.
    /// </summary>
    public IReadOnlyDictionary<string, ModuleManifest> Load(string pluginRoot)
    {
        var byId = new Dictionary<string, ModuleManifest>(StringComparer.OrdinalIgnoreCase);
        var chosenPathById = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);

        var fullRoot = Path.GetFullPath(pluginRoot);
        if (!Directory.Exists(fullRoot))
        {
            return byId;
        }

        foreach (var manifestPath in Directory.EnumerateFiles(fullRoot, ManifestFileName, SearchOption.AllDirectories))
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

            if (chosenPathById.TryGetValue(manifest.Id, out var existingPath)
                && PathSegmentCount(existingPath) <= PathSegmentCount(manifestPath))
            {
                continue;
            }

            byId[manifest.Id] = manifest;
            chosenPathById[manifest.Id] = manifestPath;
        }

        return byId;
    }

    private static ModuleManifest? LoadManifest(string manifestPath)
    {
        try
        {
            return JsonSerializer.Deserialize<ModuleManifest>(File.ReadAllText(manifestPath), JsonOptions);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            // A single unreadable/invalid manifest must not take the whole catalog down.
            return null;
        }
    }

    private static bool IsRuntimeManifestPath(string manifestPath)
    {
        var normalizedPath = Path.GetFullPath(manifestPath);

        return !normalizedPath.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase)
            && !normalizedPath.Contains($"{Path.DirectorySeparatorChar}ref{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase)
            && !normalizedPath.Contains($"{Path.DirectorySeparatorChar}refint{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase);
    }

    private static int PathSegmentCount(string path) =>
        path.Count(c => c == Path.DirectorySeparatorChar || c == Path.AltDirectorySeparatorChar);
}
