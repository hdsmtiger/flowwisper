package main

import (
    "context"
    "encoding/json"
    "net/http"
    "time"

    "github.com/go-chi/chi/v5"
    "github.com/go-chi/chi/v5/middleware"
    "go.uber.org/zap"
)

type EngineDecision struct {
    SessionID   string `json:"session_id"`
    PreferCloud bool   `json:"prefer_cloud"`
    Reason      string `json:"reason"`
}

func main() {
    logger, _ := zap.NewProduction()
    defer logger.Sync()

    r := chi.NewRouter()
    r.Use(middleware.RequestID)
    r.Use(middleware.RealIP)
    r.Use(middleware.Logger)
    r.Get("/healthz", func(w http.ResponseWriter, _ *http.Request) {
        w.Header().Set("Content-Type", "application/json")
        _, _ = w.Write([]byte(`{"status":"ok"}`))
    })

    r.Post("/decide", func(w http.ResponseWriter, r *http.Request) {
        ctx, cancel := context.WithTimeout(r.Context(), 200*time.Millisecond)
        defer cancel()

        decision := EngineDecision{
            SessionID:   "demo",
            PreferCloud: true,
            Reason:      "placeholder decision",
        }

        select {
        case <-ctx.Done():
            http.Error(w, "decision timeout", http.StatusGatewayTimeout)
            return
        default:
        }

        w.Header().Set("Content-Type", "application/json")
        _ = json.NewEncoder(w).Encode(decision)
    })

    logger.Info("hybrid router listening", zap.String("addr", ":8090"))
    http.ListenAndServe(":8090", r)
}
