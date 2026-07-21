namespace Mvf.Graph.Pipelines;

public sealed class PipelineValidationResult
{
    public IReadOnlyList<PipelineValidationIssue> Issues { get; set; } = [];

    public bool IsValid => Issues.All(issue => !string.Equals(issue.Severity, "error", StringComparison.OrdinalIgnoreCase));
}
