using MachineVisionFabric.Contracts.Packages;

namespace MachineVisionFabric.Core.Abstractions;

public interface IProductPresenceGateResolver
{
    ProductPresenceGateResolution Resolve(FabricProfileManifest manifest, string integrationsRoot);
}
