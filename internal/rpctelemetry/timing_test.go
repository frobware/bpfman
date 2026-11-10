package rpctelemetry

import (
	"bytes"
	"context"
	"encoding/json"
	"log/slog"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestUnaryPreservesResultsAndMeasuresCancelledAndFailedHandlers(t *testing.T) {
	t.Parallel()

	for _, code := range []codes.Code{codes.OK, codes.NotFound, codes.Internal} {
		t.Run(code.String(), func(t *testing.T) {
			t.Parallel()

			synctest.Test(t, func(t *testing.T) {
				var output bytes.Buffer
				interceptor := Unary(slog.New(slog.NewJSONHandler(&output, nil)))
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				response := &struct{ Secret string }{"private-response-secret"}
				original := status.Error(code, "private-status-secret")
				request := &struct{ Secret string }{"private-request-secret"}
				got, err := interceptor(ctx, request, &grpc.UnaryServerInfo{FullMethod: "/test.Service/Work"}, func(received context.Context, req any) (any, error) {
					require.Same(t, request, req)
					time.Sleep(125 * time.Millisecond)
					cancel()
					require.ErrorIs(t, received.Err(), context.Canceled)
					return response, original
				})
				require.Same(t, response, got)
				require.Equal(t, original, err)

				var event map[string]any
				require.NoError(t, json.Unmarshal(output.Bytes(), &event))
				require.Equal(t, "RPC completed", event["msg"])
				require.Equal(t, "/test.Service/Work", event["rpc_method"])
				require.Equal(t, code.String(), event["grpc_status"])
				require.Equal(t, float64(125), event["execution_ms"])
				require.Equal(t, true, event["cancellation_requested"])
				require.Positive(t, event["request_id"])
				for _, secret := range []string{"private-request-secret", "private-response-secret", "private-status-secret"} {
					require.NotContains(t, output.String(), secret)
				}
			})
		})
	}
}

func TestConcurrentListenersHaveDistinctIDsAndNoHandlerSerialization(t *testing.T) {
	t.Parallel()

	synctest.Test(t, func(t *testing.T) {
		var output lockedBuffer
		logger := slog.New(slog.NewJSONHandler(&output, nil))
		// Separate interceptor instances model management and CSI listeners.
		management := Unary(logger.With("component", "server"))
		csi := Unary(logger.With("component", "csi"))
		started := make(chan struct{}, 2)
		release := make(chan struct{})
		var workers sync.WaitGroup
		for _, interceptor := range []grpc.UnaryServerInterceptor{management, csi} {
			workers.Go(func() {
				_, err := interceptor(context.Background(), nil, &grpc.UnaryServerInfo{FullMethod: "/test.Service/Work"}, func(context.Context, any) (any, error) {
					started <- struct{}{}
					<-release
					return nil, nil
				})
				if err != nil {
					t.Errorf("handler: %v", err)
				}
			})
		}
		<-started
		<-started
		time.Sleep(50 * time.Millisecond)
		close(release)
		workers.Wait()

		decoder := json.NewDecoder(bytes.NewReader(output.Bytes()))
		ids := make(map[float64]bool)
		components := make(map[string]bool)
		for range 2 {
			var event map[string]any
			require.NoError(t, decoder.Decode(&event))
			id := event["request_id"].(float64)
			require.False(t, ids[id])
			ids[id] = true
			components[event["component"].(string)] = true
			require.Equal(t, float64(50), event["execution_ms"])
			require.Equal(t, false, event["cancellation_requested"])
		}
		require.Len(t, components, 2)
	})
}

type lockedBuffer struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func (b *lockedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.b.Write(p)
}

func (b *lockedBuffer) Bytes() []byte {
	b.mu.Lock()
	defer b.mu.Unlock()
	return bytes.Clone(b.b.Bytes())
}
