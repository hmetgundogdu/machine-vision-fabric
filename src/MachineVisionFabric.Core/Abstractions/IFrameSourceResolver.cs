using MachineVisionFabric.Contracts.Packages;

namespace MachineVisionFabric.Core.Abstractions;

public interface IFrameSourceResolver
{
    FrameSourceResolution Resolve(FabricRuntimeProfile profile, string packageRoot, string integrationsRoot);
}
