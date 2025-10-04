from fastapi.testclient import TestClient

from flowwisper_api.main import app, settings


def test_sessions_route_returns_placeholder():
    client = TestClient(app)
    response = client.get(f"/api/{settings.api_version}/sessions")
    assert response.status_code == 200
    body = response.json()
    assert body.get("sessions") == []
