using MachineVisionFabric.Contracts.Packages;

namespace MachineVisionFabric.Core.Abstractions;

public interface IEntryProfileLoader
{
    Task<FabricRuntimeProfile> LoadAsync(string packageRoot, string entryProfile, CancellationToken cancellationToken);
}
