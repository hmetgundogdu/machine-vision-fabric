namespace Mvf.Graph.Control;

public sealed class S7SignalAddress
{
    public int DataBlockNumber { get; set; } = 1;

    public int ByteOffset { get; set; } = 0;

    public int BitOffset { get; set; } = 0;
}
