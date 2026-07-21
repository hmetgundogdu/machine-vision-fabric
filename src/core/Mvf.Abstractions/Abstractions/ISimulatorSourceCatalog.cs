using Mvf.Graph.Simulation;

namespace Mvf.Abstractions;

public interface ISimulatorSourceCatalog
{
    IFrameSourceSession OpenSession(FolderSequenceSourceOptions options);
}
