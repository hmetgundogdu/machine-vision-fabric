using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using MachineVisionFabric.Contracts.Control;
using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.S7GatewayGate;

public sealed class S7GatewayGateIntegrationModule : IProductPresenceGateModule
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    public IntegrationModuleDescriptor Describe()
    {
        return IntegrationModuleDescriptorBuilder.CreateGate<S7GatewayGateOptions>(
            "mvf.s7-gateway-gate",
            "S7 Gateway Product Presence Gate",
            "0.1.0",
            "s7-gateway-product-presence-gate",
            "Reads an S7-style DB/byte/bit product presence signal through a TCP gateway.");
    }

    public IProductPresenceGate CreateGate(JsonElement configuration)
    {
        var options = configuration.ValueKind is JsonValueKind.Undefined or JsonValueKind.Null
            ? new S7GatewayGateOptions()
            : JsonSerializer.Deserialize<S7GatewayGateOptions>(configuration.GetRawText(), JsonOptions) ?? new S7GatewayGateOptions();

        return new S7GatewayGate(options);
    }

    private sealed class S7GatewayGate(S7GatewayGateOptions options) : IProductPresenceGate
    {
        public async Task<ProductPresenceDecision> EvaluateAsync(CancellationToken cancellationToken)
        {
            using var client = new TcpClient();

            using var connectCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            connectCts.CancelAfter(options.ConnectTimeoutMs);
            await client.ConnectAsync(options.Host, options.Port, connectCts.Token);

            await using var stream = client.GetStream();
            stream.ReadTimeout = options.ReadTimeoutMs;
            stream.WriteTimeout = options.ReadTimeoutMs;

            var requestLine = BuildRequestLine(options);
            var requestBytes = Encoding.UTF8.GetBytes(requestLine + "\n");
            await stream.WriteAsync(requestBytes, cancellationToken);
            await stream.FlushAsync(cancellationToken);

            using var readCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            readCts.CancelAfter(options.ReadTimeoutMs);
            using var reader = new StreamReader(stream, Encoding.UTF8, leaveOpen: true);
            var response = (await reader.ReadLineAsync(readCts.Token))?.Trim();

            if (string.IsNullOrWhiteSpace(response))
            {
                throw new InvalidOperationException("S7 gateway gate returned an empty response.");
            }

            var productPresent = ResolveProductPresent(response, options);
            return new ProductPresenceDecision(
                productPresent,
                options.SourceName,
                options.StationId,
                DateTime.UtcNow,
                $"s7-request={requestLine}; s7-response={response}");
        }

        private static string BuildRequestLine(S7GatewayGateOptions options)
        {
            return $"READ rack={options.Rack};slot={options.Slot};db={options.Address.DataBlockNumber};byte={options.Address.ByteOffset};bit={options.Address.BitOffset}";
        }

        private static bool ResolveProductPresent(string response, S7GatewayGateOptions options)
        {
            if (string.Equals(response, options.ProductPresentValue, StringComparison.OrdinalIgnoreCase))
            {
                return true;
            }

            if (string.Equals(response, options.ProductAbsentValue, StringComparison.OrdinalIgnoreCase))
            {
                return false;
            }

            if (bool.TryParse(response, out var parsedBool))
            {
                return parsedBool;
            }

            throw new InvalidOperationException(
                $"S7 gateway gate returned '{response}', which does not match present='{options.ProductPresentValue}' or absent='{options.ProductAbsentValue}'.");
        }
    }
}
