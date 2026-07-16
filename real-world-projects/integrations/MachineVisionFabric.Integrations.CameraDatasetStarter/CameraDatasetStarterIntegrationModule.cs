using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.CameraDatasetStarter;

public sealed class CameraDatasetStarterIntegrationModule : FrameSourceModuleBase<CameraDatasetStarterOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor()
    {
        return IntegrationModuleDescriptorBuilder.CreateSource<CameraDatasetStarterOptions>(
            "mvf.realworld-camera-starter",
            "Real-World Camera Dataset Starter",
            "0.1.0",
            "camera-dataset-source",
            "Project-local resident camera source starter built for real-world dataset collection work.");
    }

    protected override IFrameSourceSession OpenSession(CameraDatasetStarterOptions options, string packageRoot)
    {
        var sourceFolder = PackagePathResolver.Resolve(packageRoot, options.SourceFolder);
        return new CameraDatasetStarterSession(sourceFolder, options);
    }
}
