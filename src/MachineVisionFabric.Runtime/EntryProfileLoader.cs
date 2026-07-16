using System.Text.Json;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime;

public sealed class EntryProfileLoader : IEntryProfileLoader
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = true
    };

    public async Task<FabricRuntimeProfile> LoadAsync(string packageRoot, string entryProfile, CancellationToken cancellationToken)
    {
        var entryProfilePath = Path.Combine(packageRoot, entryProfile);
        if (!File.Exists(entryProfilePath))
        {
            throw new FileNotFoundException("Fabric entry profile was not found.", entryProfilePath);
        }

        await using var stream = File.OpenRead(entryProfilePath);
        var profile = await JsonSerializer.DeserializeAsync<FabricRuntimeProfile>(stream, JsonOptions, cancellationToken);

        return profile ?? throw new InvalidOperationException("Fabric entry profile could not be deserialized.");
    }
}
