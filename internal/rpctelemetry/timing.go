// Package rpctelemetry provides diagnostic timing for synchronous unary RPCs.
package rpctelemetry

import (
	"context"
	"log/slog"
	"sync/atomic"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/status"
)

// Shared by the management and CSI servers for this process lifetime.
var nextRequest atomic.Uint64

// Unary measures the handler, including its lock waits and downstream work.
// grpc-go decodes before this boundary and encodes/sends after it. There is no
// application worker queue here; do not invent queue or delivery observations.
// The returned response and error are preserved even if cancellation races them.
func Unary(logger *slog.Logger) grpc.UnaryServerInterceptor {
	return func(ctx context.Context, req any, info *grpc.UnaryServerInfo, handler grpc.UnaryHandler) (any, error) {
		requestID := nextRequest.Add(1)
		start := time.Now()
		resp, err := handler(ctx, req)
		elapsed := time.Since(start)

		// This temporary diagnostic emits every completion at INFO so an INFO
		// capture accounts for successful reads and expected error returns too.
		// Never render req, resp or the free-form status/error message here.
		logger.InfoContext(ctx, "RPC completed",
			"request_id", requestID,
			"rpc_method", info.FullMethod,
			"grpc_status", status.Code(err).String(),
			"execution_ms", float64(elapsed)/float64(time.Millisecond),
			"cancellation_requested", ctx.Err() != nil,
		)
		return resp, err
	}
}
