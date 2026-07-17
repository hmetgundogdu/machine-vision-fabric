using System.Text.Json;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Contracts.Pipelines;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Pipelines;

public sealed class PackagePipelineDefinitionProvider(
    DatasetCaptureCompatibilityPipelineFactory compatibilityPipelineFactory) : IPipelineDefinitionProvider
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = true
    };

    public async Task<ResolvedPipelineDefinition> LoadAsync(
        string packageRoot,
        FabricProfileManifest manifest,
        FabricRuntimeProfile profile,
        CancellationToken cancellationToken)
    {
        if (!string.IsNullOrWhiteSpace(manifest.PipelineDefinition))
        {
            var pipelinePath = Path.Combine(packageRoot, manifest.PipelineDefinition);
            if (!File.Exists(pipelinePath))
            {
                throw new FileNotFoundException("Pipeline definition file was not found.", pipelinePath);
            }

            await using var stream = File.OpenRead(pipelinePath);
            var definition = await JsonSerializer.DeserializeAsync<PipelineDefinition>(stream, JsonOptions, cancellationToken);

            return new ResolvedPipelineDefinition
            {
                Definition = definition ?? throw new InvalidOperationException("Pipeline definition could not be deserialized."),
                Source = manifest.PipelineDefinition,
                IsSynthetic = false
            };
        }

        return new ResolvedPipelineDefinition
        {
            Definition = compatibilityPipelineFactory.Create(manifest, profile),
            Source = "synthetic-compatibility-graph",
            IsSynthetic = true
        };
    }
}
