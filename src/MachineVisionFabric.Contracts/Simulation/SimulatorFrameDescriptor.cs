namespace MachineVisionFabric.Contracts.Simulation;

public sealed record SimulatorFrameDescriptor(
    string CameraId,
    int SequenceNumber,
    string SourcePath,
    string FileName);
