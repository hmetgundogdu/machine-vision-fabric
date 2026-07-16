using MachineVisionFabric.Contracts.Control;

namespace MachineVisionFabric.Core.Abstractions;

public interface IProductPresenceGate
{
    Task<ProductPresenceDecision> EvaluateAsync(CancellationToken cancellationToken);
}
