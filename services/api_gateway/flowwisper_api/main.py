from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware

from .config import get_settings
from .routers import sessions

settings = get_settings()

app = FastAPI(title="Flowwisper API Gateway", version="0.1.0")
app.include_router(sessions.router, prefix=f"/api/{settings.api_version}", tags=["sessions"])

app.add_middleware(
    CORSMiddleware,
    allow_origins=settings.allow_origins,
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)


@app.get("/healthz")
async def health_check() -> dict[str, str]:
    """Lightweight health probe."""

    return {"status": "ok"}
