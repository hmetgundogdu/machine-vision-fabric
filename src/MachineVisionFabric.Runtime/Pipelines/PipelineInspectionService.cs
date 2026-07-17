using MachineVisionFabric.Contracts.Inspection;
using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Pipelines;

public sealed class PipelineInspectionService(
    IPackageManifestLoader packageManifestLoader,
    IEntryProfileLoader entryProfileLoader,
    IPipelineDefinitionProvider pipelineDefinitionProvider,
    IPipelineDefinitionValidator pipelineDefinitionValidator,
    IIntegrationModuleLoader integrationModuleLoader) : IPipelineInspectionService
{
    public async Task<PipelineInspectionReport> InspectAsync(
        string packageRoot,
        string integrationsRoot,
        CancellationToken cancellationToken)
    {
        var manifest = await packageManifestLoader.LoadAsync(packageRoot, cancellationToken);
        var profile = await entryProfileLoader.LoadAsync(packageRoot, manifest.EntryProfile, cancellationToken);
        var resolvedPipeline = await pipelineDefinitionProvider.LoadAsync(packageRoot, manifest, profile, cancellationToken);
        var validation = pipelineDefinitionValidator.Validate(resolvedPipeline.Definition);
        var moduleDescriptors = integrationModuleLoader.LoadModules(integrationsRoot)
            .Select(module => module.Describe())
            .OrderBy(descriptor => descriptor.ModuleId, StringComparer.OrdinalIgnoreCase)
            .ToArray();

        return new PipelineInspectionReport
        {
            PackageRoot = packageRoot,
            IntegrationsRoot = integrationsRoot,
            Manifest = manifest,
            Profile = profile,
            Pipeline = resolvedPipeline.Definition,
            PipelineSource = resolvedPipeline.Source,
            PipelineIsSynthetic = resolvedPipeline.IsSynthetic,
            Validation = validation,
            AvailableModules = moduleDescriptors
        };
    }
}
