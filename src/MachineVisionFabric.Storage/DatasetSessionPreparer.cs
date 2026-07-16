using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Storage;

public sealed class DatasetSessionPreparer : IDatasetSessionPreparer
{
    public string PrepareSessionRoot(string datasetRoot, string sessionPrefix, bool createSession)
    {
        if (!createSession)
        {
            return datasetRoot;
        }

        var sessionRoot = CreateUniqueSessionRoot(datasetRoot, sessionPrefix);

        Directory.CreateDirectory(sessionRoot);
        Directory.CreateDirectory(Path.Combine(sessionRoot, "images"));
        Directory.CreateDirectory(Path.Combine(sessionRoot, "metadata"));
        Directory.CreateDirectory(Path.Combine(sessionRoot, "rejected"));

        return sessionRoot;
    }

    private static string CreateUniqueSessionRoot(string datasetRoot, string sessionPrefix)
    {
        for (var attempt = 0; attempt < 1000; attempt++)
        {
            var suffix = attempt == 0 ? string.Empty : $"-{attempt:000}";
            var sessionName = $"{sessionPrefix}-{DateTime.UtcNow:yyyyMMdd-HHmmss-fff}{suffix}";
            var sessionRoot = Path.Combine(datasetRoot, sessionName);

            if (!Directory.Exists(sessionRoot))
            {
                return sessionRoot;
            }
        }

        throw new IOException("Could not allocate a unique dataset session directory.");
    }
}
