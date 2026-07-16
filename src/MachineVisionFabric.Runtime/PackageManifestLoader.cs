using System.Text.Json;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime;

public sealed class PackageManifestLoader : IPackageManifestLoader
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = true
    };

    public async Task<FabricProfileManifest> LoadAsync(string packageRoot, CancellationToken cancellationToken)
    {
        var manifestPath = Path.Combine(packageRoot, "manifest.json");
        if (!File.Exists(manifestPath))
        {
            throw new FileNotFoundException("Fabric package manifest was not found.", manifestPath);
        }

        await using var stream = File.OpenRead(manifestPath);
        var manifest = await JsonSerializer.DeserializeAsync<FabricProfileManifest>(stream, JsonOptions, cancellationToken);

        return manifest ?? throw new InvalidOperationException("Fabric package manifest could not be deserialized.");
    }
}
