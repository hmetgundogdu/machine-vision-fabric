using MachineVisionFabric.Contracts.Execution;

namespace MachineVisionFabric.Core.Abstractions;

/// <summary>
/// Discriminated union representing a value on a typed port.
/// A port carries either a data frame or a control signal — never both.
/// </summary>
public sealed class PortValue
{
    private PortValue() { }

    public IFrameEnvelope? Frame { get; private init; }

    public ControlSignal? Control { get; private init; }

    public bool IsFrame => Frame is not null;

    public bool IsControl => Control is not null;

    public static PortValue FromFrame(IFrameEnvelope frame)
    {
        ArgumentNullException.ThrowIfNull(frame);
        return new PortValue { Frame = frame };
    }

    public static PortValue FromControl(ControlSignal signal)
    {
        ArgumentNullException.ThrowIfNull(signal);
        return new PortValue { Control = signal };
    }
}
