FROM mcr.microsoft.com/devcontainers/base:noble

RUN apt update &&\
    apt install -y mold &&\
    apt clean &&\
    rm -rf /var/lib/apt/lists/*

