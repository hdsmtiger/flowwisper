from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_prefix="FLOWWISPER_", env_file=".env", env_file_encoding="utf-8")

    api_version: str = "v1"
    allow_origins: list[str] = ["http://localhost:1420"]
    auth_required: bool = False


def get_settings() -> Settings:
    return Settings()
