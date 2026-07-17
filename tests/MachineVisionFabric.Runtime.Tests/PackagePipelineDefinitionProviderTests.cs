using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Contracts.Pipelines;
using MachineVisionFabric.Runtime.Pipelines;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class PackagePipelineDefinitionProviderTests
{
    [Fact]
    public async Task LoadAsync_BuildsSyntheticCompatibilityGraph_WhenPackageHasNoPipelineFile()
    {
        var provider = new PackagePipelineDefinitionProvider(new DatasetCaptureCompatibilityPipelineFactory());
        var validator = new PipelineDefinitionValidator();

        var manifest = new FabricProfileManifest
        {
            Name = "compat-package",
            ProductPresenceGate = new ProductPresenceGateBinding
            {
                Mode = "builtin"
            }
        };

        var profile = new FabricRuntimeProfile
        {
            Name = "compat-profile",
            Source = new SourceBinding
            {
                Mode = "builtin"
            }
        };

        var resolved = await provider.LoadAsync("C:\\temp", manifest, profile, CancellationToken.None);
        var validation = validator.Validate(resolved.Definition);

        Assert.True(resolved.IsSynthetic);
        Assert.Equal("synthetic-compatibility-graph", resolved.Source);
        Assert.True(validation.IsValid);
        Assert.Contains(resolved.Definition.Nodes, node => node.Kind == "runtime-builtin" && node.BuiltinType == "folder-sequence-source");
        Assert.Contains(resolved.Definition.Nodes, node => node.Kind == "embedded-primitive" && node.PrimitiveType == "if");
    }

    [Fact]
    public async Task LoadAsync_LoadsExplicitPipelineDefinition_WhenManifestDeclaresPipelineFile()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);

        try
        {
            await File.WriteAllTextAsync(
                Path.Combine(root, "pipeline.json"),
                """
                {
                  "name": "explicit-pipeline",
                  "nodes": [],
                  "edges": []
                }
                """);

            var provider = new PackagePipelineDefinitionProvider(new DatasetCaptureCompatibilityPipelineFactory());
            var manifest = new FabricProfileManifest
            {
                PipelineDefinition = "pipeline.json"
            };
            var profile = new FabricRuntimeProfile();

            var resolved = await provider.LoadAsync(root, manifest, profile, CancellationToken.None);

            Assert.False(resolved.IsSynthetic);
            Assert.Equal("pipeline.json", resolved.Source);
            Assert.Equal("explicit-pipeline", resolved.Definition.Name);
        }
        finally
        {
            if (Directory.Exists(root))
            {
                Directory.Delete(root, recursive: true);
            }
        }
    }
}
