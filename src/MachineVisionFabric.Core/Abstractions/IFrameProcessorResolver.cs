using MachineVisionFabric.Contracts.Packages;

namespace MachineVisionFabric.Core.Abstractions;

public interface IFrameProcessorResolver
{
    FrameProcessorResolution Resolve(FabricProfileManifest manifest, string integrationsRoot);
}
