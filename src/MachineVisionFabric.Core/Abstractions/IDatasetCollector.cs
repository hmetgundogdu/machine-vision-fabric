using MachineVisionFabric.Contracts.Dataset;
using MachineVisionFabric.Contracts.Packages;

namespace MachineVisionFabric.Core.Abstractions;

public interface IDatasetCollector
{
    Task<DatasetCollectionResult> CollectAsync(
        string sessionRoot,
        FabricProfileManifest manifest,
        int declaredCameraCount,
        IProductPresenceGate productPresenceGate,
        IFrameProcessor? frameProcessor,
        IFrameSourceSession frameSourceSession,
        CancellationToken cancellationToken);
}
