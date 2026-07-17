using MachineVisionFabric.Contracts.Inspection;

namespace MachineVisionFabric.Core.Abstractions;

public interface IPipelineInspectionService
{
    Task<PipelineInspectionReport> InspectAsync(
        string packageRoot,
        string integrationsRoot,
        CancellationToken cancellationToken);
}
