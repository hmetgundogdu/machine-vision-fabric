using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.ResidentCameraStub;

public sealed class ResidentCameraStubIntegrationModule : FrameSourceModuleBase<ResidentCameraStubOptions>
{
    protected override MachineVisionFabric.Contracts.Integrations.IntegrationModuleDescriptor BuildDescriptor()
    {
        return IntegrationModuleDescriptorBuilder.CreateSource<ResidentCameraStubOptions>(
            "mvf.resident-camera-stub",
            "Resident Camera Stub",
            "0.1.0",
            "resident-camera-source",
            "Runs a bounded, resident frame producer that simulates a live camera adapter.");
    }

    protected override IFrameSourceSession OpenSession(ResidentCameraStubOptions options, string packageRoot)
    {
        var sourceFolder = PackagePathResolver.Resolve(packageRoot, options.SourceFolder);

        return new ResidentCameraStubSession(sourceFolder, options);
    }
}
