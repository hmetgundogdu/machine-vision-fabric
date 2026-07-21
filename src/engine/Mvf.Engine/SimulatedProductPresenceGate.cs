using Mvf.Graph.Control;
using Mvf.Abstractions;

namespace Mvf.Engine;

public sealed class SimulatedProductPresenceGate(SimulatedPlcGateOptions gate) : IProductPresenceGate
{
    private int evaluationCount;
    private readonly DateTime startedAtUtc = DateTime.UtcNow;

    public Task<ProductPresenceDecision> EvaluateAsync(CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();

        var productPresent = ResolveProductPresent();

        return Task.FromResult(new ProductPresenceDecision(
            gate.Enabled && productPresent,
            gate.SourceName,
            gate.StationId,
            DateTime.UtcNow,
            gate.Details));
    }

    private bool ResolveProductPresent()
    {
        if (gate.DelayBeforePresentMs > 0)
        {
            var elapsed = DateTime.UtcNow - startedAtUtc;
            if (elapsed < TimeSpan.FromMilliseconds(gate.DelayBeforePresentMs))
            {
                return false;
            }

            return true;
        }

        if (gate.ProductPresentSequence.Count == 0)
        {
            return gate.ProductPresent;
        }

        var currentIndex = Interlocked.Increment(ref evaluationCount) - 1;
        if (currentIndex < gate.ProductPresentSequence.Count)
        {
            return gate.ProductPresentSequence[currentIndex];
        }

        return gate.HoldLastSequenceValue
            ? gate.ProductPresentSequence[^1]
            : gate.ProductPresent;
    }
}
