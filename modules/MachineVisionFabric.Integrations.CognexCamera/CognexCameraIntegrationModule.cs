using Mvf.Graph.Integrations;
using Mvf.Abstractions;
using Mvf.Sdk;

namespace MachineVisionFabric.Integrations.CognexCamera;

/// <summary>
/// Integration module for Cognex In-Sight cameras via the HMI WebSocket API.
/// Supports passive-listen and manual-trigger acquisition modes.
/// Runs continuously — the pipeline executor controls when to stop.
/// </summary>
public sealed class CognexCameraIntegrationModule : FrameSourceModuleBase<CognexCameraOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor() =>
        IntegrationModuleDescriptorBuilder.CreateSource<CognexCameraOptions>(
            moduleId:       "mvf.realworld-cognex-camera",
            displayName:    "Cognex In-Sight Camera",
            version:        "1.0.0",
            capabilityName: "cognex-camera-source",
            description:    "Cognex In-Sight HMI WebSocket source. Connects, triggers, and streams frames continuously.");

    protected override IFrameSourceSession OpenSession(CognexCameraOptions options, string packageRoot) =>
        new CognexCameraSession(options);
}
