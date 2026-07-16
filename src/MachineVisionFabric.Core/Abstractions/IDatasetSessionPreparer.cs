namespace MachineVisionFabric.Core.Abstractions;

public interface IDatasetSessionPreparer
{
    string PrepareSessionRoot(string datasetRoot, string sessionPrefix, bool createSession);
}
