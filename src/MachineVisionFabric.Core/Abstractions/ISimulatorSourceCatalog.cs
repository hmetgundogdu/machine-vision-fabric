using MachineVisionFabric.Contracts.Simulation;

namespace MachineVisionFabric.Core.Abstractions;

public interface ISimulatorSourceCatalog
{
    IFrameSourceSession OpenSession(FolderSequenceSourceOptions options);
}
