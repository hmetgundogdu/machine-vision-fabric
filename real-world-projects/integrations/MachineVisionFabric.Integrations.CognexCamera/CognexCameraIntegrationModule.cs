using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.CognexCamera;

public sealed class CognexCameraIntegrationModule : FrameSourceModuleBase<CognexCameraOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor()
    {
        return IntegrationModuleDescriptorBuilder.CreateSource<CognexCameraOptions>(
            "mvf.realworld-cognex-camera",
            "Cognex Camera HMI Source",
            "0.1.0",
            "cognex-camera-source",
            "Resident Cognex In-Sight HMI-backed source module for real-world dataset collection.");
    }

    protected override IFrameSourceSession OpenSession(CognexCameraOptions options, string packageRoot)
    {
        return new CognexCameraSession(options);
    }
}
