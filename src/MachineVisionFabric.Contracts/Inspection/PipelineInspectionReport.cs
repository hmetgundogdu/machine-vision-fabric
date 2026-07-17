using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Contracts.Pipelines;

namespace MachineVisionFabric.Contracts.Inspection;

public sealed class PipelineInspectionReport
{
    public required string PackageRoot { get; init; }

    public required string IntegrationsRoot { get; init; }

    public required FabricProfileManifest Manifest { get; init; }

    public required FabricRuntimeProfile Profile { get; init; }

    public required PipelineDefinition Pipeline { get; init; }

    public required string PipelineSource { get; init; }

    public required bool PipelineIsSynthetic { get; init; }

    public required PipelineValidationResult Validation { get; init; }

    public required IReadOnlyList<IntegrationModuleDescriptor> AvailableModules { get; init; }
}
