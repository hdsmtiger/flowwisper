from fastapi import APIRouter

router = APIRouter()


@router.get("/sessions")
async def list_sessions() -> dict[str, list[dict[str, str]]]:
    """占位列表接口，后续接入 Sync Service。"""

    return {"sessions": []}


@router.post("/sessions")
async def create_session() -> dict[str, str]:
    """创建语音会话占位接口。"""

    return {"session_id": "demo"}
