using MachineVisionFabric.Contracts.Packages;

namespace MachineVisionFabric.Core.Abstractions;

public interface IPackageManifestLoader
{
    Task<FabricProfileManifest> LoadAsync(string packageRoot, CancellationToken cancellationToken);
}
