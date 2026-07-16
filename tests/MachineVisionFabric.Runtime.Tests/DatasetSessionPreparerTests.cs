using MachineVisionFabric.Storage;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class DatasetSessionPreparerTests
{
    [Fact]
    public void PrepareSessionRoot_CreatesUniqueRootsAcrossFastSuccessiveCalls()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);

        try
        {
            var preparer = new DatasetSessionPreparer();

            var sessionA = preparer.PrepareSessionRoot(root, "session", createSession: true);
            var sessionB = preparer.PrepareSessionRoot(root, "session", createSession: true);

            Assert.NotEqual(sessionA, sessionB);
            Assert.True(Directory.Exists(sessionA));
            Assert.True(Directory.Exists(sessionB));
        }
        finally
        {
            if (Directory.Exists(root))
            {
                Directory.Delete(root, recursive: true);
            }
        }
    }
}
