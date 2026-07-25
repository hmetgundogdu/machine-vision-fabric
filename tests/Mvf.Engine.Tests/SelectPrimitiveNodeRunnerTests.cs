using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Engine.Execution.NodeRunners;
using Mvf.Graph.Execution;
using Mvf.Graph.Values;

namespace Mvf.Engine.Tests;

public sealed class SelectPrimitiveNodeRunnerTests
{
    private static JsonArray Cameras() =>
    [
        new JsonObject { ["serial"] = "ABC123", ["protocol"] = "gige" },
        new JsonObject { ["serial"] = "DEF456", ["protocol"] = "gige" },
        new JsonObject { ["serial"] = "GHI789", ["protocol"] = "usb" }
    ];

    private static NodeExecutionInputs Inputs(JsonArray? items, JsonNode? criterion = null)
    {
        var values = new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase);

        if (items is not null)
        {
            values["items"] = PortValue.FromControl(ControlSignal.FromList(items));
        }

        if (criterion is not null)
        {
            values["criterion"] = PortValue.FromControl(ControlSignal.FromValue(criterion));
        }

        return new NodeExecutionInputs(values);
    }

    [Fact]
    public async Task Execute_ModeOne_SelectsByProperty()
    {
        var runner = new SelectPrimitiveNodeRunner(
            "pickCam", SelectMode.One, JsonValue.Create("DEF456"), by: "serial");

        var result = await runner.ExecuteAsync(Inputs(Cameras()), CancellationToken.None);

        var selected = Assert.IsType<JsonObject>(result.Get("selected")!.Control!.Payload);
        Assert.Equal("DEF456", selected["serial"]!.GetValue<string>());
    }

    [Fact]
    public async Task Execute_ModeMany_NarrowsTheCollection()
    {
        var runner = new SelectPrimitiveNodeRunner(
            "gigeOnly", SelectMode.Many, new JsonObject { ["protocol"] = "gige" }, by: null);

        var result = await runner.ExecuteAsync(Inputs(Cameras()), CancellationToken.None);

        var selected = Assert.IsType<JsonArray>(result.Get("selected")!.Control!.Payload);
        Assert.Equal(2, selected.Count);
        Assert.All(selected, item => Assert.Equal("gige", item!["protocol"]!.GetValue<string>()));
    }

    [Fact]
    public async Task Execute_ModeMany_EmitsAnEmptyCollectionWhenNothingMatches()
    {
        var runner = new SelectPrimitiveNodeRunner(
            "none", SelectMode.Many, new JsonObject { ["protocol"] = "camera-link" }, by: null);

        var result = await runner.ExecuteAsync(Inputs(Cameras()), CancellationToken.None);

        var selected = Assert.IsType<JsonArray>(result.Get("selected")!.Control!.Payload);
        Assert.Empty(selected);
    }

    [Fact]
    public async Task Execute_ModeOne_EmitsNothingWhenNothingMatches()
    {
        var runner = new SelectPrimitiveNodeRunner(
            "pickCam", SelectMode.One, JsonValue.Create("NOPE"), by: "serial");

        var result = await runner.ExecuteAsync(Inputs(Cameras()), CancellationToken.None);

        Assert.False(result.HasOutput);
    }

    [Fact]
    public async Task Execute_CriterionOnTheEdgeWinsOverTheStaticOne()
    {
        var runner = new SelectPrimitiveNodeRunner(
            "pickCam", SelectMode.One, JsonValue.Create("ABC123"), by: "serial");

        var result = await runner.ExecuteAsync(
            Inputs(Cameras(), JsonValue.Create("GHI789")), CancellationToken.None);

        var selected = Assert.IsType<JsonObject>(result.Get("selected")!.Control!.Payload);
        Assert.Equal("GHI789", selected["serial"]!.GetValue<string>());
    }

    [Fact]
    public async Task Execute_ScalarElements_CompareDirectly()
    {
        JsonArray sizes = [1, 2, 3];
        var runner = new SelectPrimitiveNodeRunner("two", SelectMode.One, JsonValue.Create(2), by: null);

        var result = await runner.ExecuteAsync(Inputs(sizes), CancellationToken.None);

        Assert.Equal(2, result.Get("selected")!.Control!.Payload!.GetValue<int>());
    }

    [Fact]
    public async Task Execute_WithoutItems_EmitsNothing()
    {
        var runner = new SelectPrimitiveNodeRunner(
            "pickCam", SelectMode.One, JsonValue.Create("ABC123"), by: "serial");

        var result = await runner.ExecuteAsync(Inputs(items: null), CancellationToken.None);

        Assert.False(result.HasOutput);
    }

    [Fact]
    public async Task Execute_WithoutAnyCriterion_EmitsNothing()
    {
        var runner = new SelectPrimitiveNodeRunner("pickCam", SelectMode.One, staticCriterion: null, by: "serial");

        var result = await runner.ExecuteAsync(Inputs(Cameras()), CancellationToken.None);

        Assert.False(result.HasOutput);
    }
}
