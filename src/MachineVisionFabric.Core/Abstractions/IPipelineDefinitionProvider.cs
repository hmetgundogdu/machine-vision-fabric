using MachineVisionFabric.Contracts.Packages;

namespace MachineVisionFabric.Core.Abstractions;

public interface IPipelineDefinitionProvider
{
    Task<ResolvedPipelineDefinition> LoadAsync(
        string packageRoot,
        FabricProfileManifest manifest,
        FabricRuntimeProfile profile,
        CancellationToken cancellationToken);
}
