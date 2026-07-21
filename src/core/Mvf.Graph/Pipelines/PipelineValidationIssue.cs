namespace Mvf.Graph.Pipelines;

public sealed class PipelineValidationIssue
{
    public string Code { get; set; } = "pipeline.issue";

    public string Severity { get; set; } = "error";

    public string Message { get; set; } = string.Empty;

    public string? NodeId { get; set; }

    public string? EdgeId { get; set; }
}
