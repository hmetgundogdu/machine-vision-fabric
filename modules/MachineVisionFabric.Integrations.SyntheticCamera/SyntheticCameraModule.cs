using Mvf.Abstractions;
using Mvf.Graph.Integrations;
using Mvf.Sdk;

namespace MachineVisionFabric.Integrations.SyntheticCamera;

/// <summary>
/// A hardware-free camera that produces real images: a lit conveyor belt with parts crossing it, some
/// defective (the Simulator-First principle).
///
/// <para>It exists because the other simulator sources replay <i>files</i> — fine for exercising a graph,
/// but every frame is an opaque blob, so nothing downstream can be tested against actual pixels. This one
/// declares a typed 2-D 8-bit <see cref="PayloadDescriptor"/>, which is what lets a worker take a real
/// numpy view, a classifier decide on brightness that means something, and a viewer show the picture.</para>
/// </summary>
public sealed class SyntheticCameraModule : FrameSourceModuleBase<SyntheticCameraOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor() =>
        IntegrationModuleDescriptorBuilder.CreateSource<SyntheticCameraOptions>(
            moduleId:       "mvf.synthetic-camera",
            displayName:    "Synthetic Belt Camera",
            version:        "1.0.0",
            capabilityName: "synthetic-belt-camera",
            description:    "Generates 8-bit grayscale frames of parts moving on a belt — a hardware-free camera with real pixels.");

    protected override IFrameSourceSession OpenSession(SyntheticCameraOptions options, string packageRoot) =>
        new SyntheticCameraSession(options);
}
