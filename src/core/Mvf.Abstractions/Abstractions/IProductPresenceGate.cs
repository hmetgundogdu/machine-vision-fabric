using Mvf.Graph.Control;

namespace Mvf.Abstractions;

public interface IProductPresenceGate
{
    Task<ProductPresenceDecision> EvaluateAsync(CancellationToken cancellationToken);
}
