using Mvf.Abstractions;
using Mvf.Engine.Execution.NodeRunners;

namespace Mvf.Engine.Tests;

public sealed class LoopPrimitiveNodeRunnerTests
{
    [Fact]
    public async Task Execute_EmitsNothing_ItIsAPortlessMarker()
    {
        // The loop carries no data — it only declares that the run repeats and that a pause control exists.
        // The dataflow runs around it; the runner itself does nothing in the cycle.
        var runner = new LoopPrimitiveNodeRunner("cycle");

        var result = await runner.ExecuteAsync(NodeExecutionInputs.Empty, CancellationToken.None);

        Assert.False(result.HasOutput);
    }
}
