namespace MachineVisionFabric.Core.Abstractions;

public sealed record ProductPresenceGateResolution(
    IProductPresenceGate Gate,
    string Strategy,
    string Source);
