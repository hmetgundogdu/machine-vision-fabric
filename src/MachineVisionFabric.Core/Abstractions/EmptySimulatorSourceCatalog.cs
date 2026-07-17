using MachineVisionFabric.Contracts.Simulation;

namespace MachineVisionFabric.Core.Abstractions;

/// <summary>
/// Default no-op simulator source catalog. The folder-sequence simulator
/// sources were removed together with the example projects; production
/// pipelines use integration-module sources (e.g. the Cognex camera).
/// Kept so the runtime's DI graph resolves without an example dependency.
/// </summary>
public sealed class EmptySimulatorSourceCatalog : ISimulatorSourceCatalog
{
    public IFrameSourceSession OpenSession(FolderSequenceSourceOptions options) =>
        throw new NotSupportedException(
            "Simulator sources are not available in this build. " +
            "Use an integration-module source (e.g. mvf.realworld-cognex-camera) instead.");
}
