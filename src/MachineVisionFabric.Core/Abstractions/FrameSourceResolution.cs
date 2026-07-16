namespace MachineVisionFabric.Core.Abstractions;

public sealed record FrameSourceResolution(
    IFrameSourceSession Session,
    string Strategy,
    string Source);
