using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class IntegrationModuleDescriptorBuilderTests
{
    [Fact]
    public void CreateSource_EmitsTypedOutputPortMetadata()
    {
        var descriptor = IntegrationModuleDescriptorBuilder.CreateSource<FakeOptions>(
            "mvf.fake-source",
            "Fake Source",
            "0.1.0",
            "frame-source",
            "Test source");

        var capability = Assert.Single(descriptor.Capabilities);

        Assert.Equal(IntegrationCapabilityKind.Source, capability.Kind);
        Assert.Empty(capability.Inputs);
        var output = Assert.Single(capability.Outputs);
        Assert.Equal("frame", output.Name);
        Assert.Equal("data", output.Channel);
        Assert.Equal("frame", output.DataType);
    }

    [Fact]
    public void CreateGate_EmitsTypedControlOutputPortMetadata()
    {
        var descriptor = IntegrationModuleDescriptorBuilder.CreateGate<FakeOptions>(
            "mvf.fake-gate",
            "Fake Gate",
            "0.1.0",
            "presence-gate",
            "Test gate");

        var capability = Assert.Single(descriptor.Capabilities);

        Assert.Equal(IntegrationCapabilityKind.Gate, capability.Kind);
        var output = Assert.Single(capability.Outputs);
        Assert.Equal("productPresent", output.Name);
        Assert.Equal("control", output.Channel);
        Assert.Equal("boolean-gate", output.DataType);
    }

    [Fact]
    public void CreateProcessor_EmitsTypedInputAndOutputPortMetadata()
    {
        var descriptor = IntegrationModuleDescriptorBuilder.CreateProcessor<FakeOptions>(
            "mvf.fake-processor",
            "Fake Processor",
            "0.1.0",
            "frame-processor",
            "Test processor");

        var capability = Assert.Single(descriptor.Capabilities);

        var input = Assert.Single(capability.Inputs);
        var output = Assert.Single(capability.Outputs);

        Assert.Equal("frame", input.Name);
        Assert.Equal("data", input.Channel);
        Assert.Equal("frame", input.DataType);
        Assert.Equal("frame", output.Name);
        Assert.Equal("data", output.Channel);
        Assert.Equal("frame", output.DataType);
    }

    private sealed class FakeOptions;
}
