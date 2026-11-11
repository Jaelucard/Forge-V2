import json
import os
import subprocess
import sys

if sys.argv[1:] == ["--version"]:
    print("grok fixture-1 (not a live provider)")
    sys.exit(0)

assert sys.argv[1:] == ["agent", "--always-approve", "--no-leader", "stdio"]
assert "XAI_API_KEY" not in os.environ
assert "GROK_CODE_XAI_API_KEY" not in os.environ
session = "new-session"
prompt_id = None
permission_answered = False


def send(value):
    print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)


def result(request, value):
    send({"id": request["id"], "result": value})


def update(kind, **data):
    send({"method": "session/update", "params": {"sessionId": session, "update": {"sessionUpdate": kind, **data}}})


def finish():
    update("agent_message_chunk", content={"type": "text", "text": "Forge"})
    update("turn_completed", usage={"inputTokens": 30, "outputTokens": 4, "costUsdTicks": 10000000})
    send({"id": prompt_id, "result": {"stopReason": "end_turn"}})


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        if MODE == "wrong-id":
            send({"id": 999, "result": {}})
        elif MODE == "malformed":
            print("not-json", flush=True)
        elif MODE == "oversized":
            print("x" * 1048577, flush=True)
        else:
            result(request, {"protocolVersion": 1, "agentCapabilities": {"loadSession": MODE != "no-resume"}, "authMethods": [{"id": "xai.api_key"}, {"id": "cached_token"}], "_meta": {"defaultAuthMethodId": "xai.api_key" if MODE == "api-key-default" else "cached_token"}})
    elif method == "authenticate":
        assert request["params"]["methodId"] == "cached_token"
        if MODE == "auth-failure":
            send({"id": request["id"], "error": {"code": -32000, "message": "Fixture sign-in required"}})
        else:
            result(request, {})
    elif method == "session/new":
        assert MODE not in ("auth-failure", "no-resume")
        assert os.path.isabs(request["params"]["cwd"])
        result(request, {"sessionId": session})
    elif method == "session/load":
        session = request["params"]["sessionId"]
        if MODE == "load-failure":
            send({"id": request["id"], "error": {"code": -32000, "message": "Fixture session is unavailable"}})
        else:
            update("agent_message_chunk", content={"type": "text", "text": "REPLAY must not duplicate saved history"})
            result(request, {})
    elif method == "session/prompt":
        prompt_id = request["id"]
        assert request["params"]["sessionId"] == session
        if MODE == "exit-during-prompt":
            sys.exit(2)
        elif MODE == "lingering-child":
            child = subprocess.Popen([sys.executable, "-c", "import os,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print(os.getpid(),flush=True); time.sleep(60)"], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
            child_pid = child.stdout.readline().strip()
            update("agent_message_chunk", content={"type": "text", "text": "child-pid:" + child_pid})
            finish()
        elif MODE in ("cancel", "cancel-no-ack"):
            update("agent_message_chunk", content={"type": "text", "text": "cancel-ready"})
        elif MODE in ("user-input", "user-input-wrapped"):
            params = {"sessionId": session, "toolCallId": "ask-1", "mode": "default", "questions": [{"id": "first", "question": "Choose several", "multiSelect": True, "options": [{"label": "A"}, {"label": "B"}]}, {"id": "second", "question": "Add context", "options": []}]}
            if MODE == "user-input-wrapped":
                params = {"method": "x.ai/ask_user_question", "params": params}
            send({"id": 17, "method": "_x.ai/ask_user_question" if MODE == "user-input-wrapped" else "x.ai/ask_user_question", "params": params})
        else:
            update("agent_message_chunk", content={"type": "text", "text": "Hello "})
            send({"id": "unsupported", "method": "fs/read_text_file", "params": {"path": "/private/file"}})
            send({"id": "native-question", "method": "session/request_permission", "params": {"sessionId": session, "toolCall": {"title": "Fixture permission only"}, "options": [{"optionId": "allow-42", "name": "Allow fixture", "kind": "allow_once"}]}})
    elif method == "session/cancel":
        assert request["params"]["_meta"]["cancelSubagents"] is True
        if MODE != "cancel-no-ack":
            send({"id": prompt_id, "result": {"stopReason": "cancelled"}})
    elif request.get("id") == "unsupported":
        assert request["error"]["code"] == -32601
    elif request.get("id") == "native-question":
        assert request["result"]["outcome"] == {"outcome": "selected", "optionId": "allow-42"}
        finish()
    elif request.get("id") == 17:
        assert request["result"]["outcome"] == "accepted"
        assert request["result"]["answers"] == {"Choose several": ["A", "B"], "Add context": ["Other"]}
        assert request["result"]["annotations"] == {"Add context": {"notes": "My custom answer"}}
        finish()
    else:
        raise AssertionError(request)
