"""割り込み発言・CSV テンプレート/一括登録・メール設定のテスト（実機。import 時に e2e_test も実行される）
使い方: E2E_HOST=10.40.0.13 E2E_ADMIN_PW=<admin のパスワード> python tools/feature_test.py
注意: テスト用ユーザを数人登録して無効化する。メール設定は最後に無効へ戻す
環境変数: E2E_HOST, E2E_ADMIN_PW
"""
import asyncio, json, os, sys, time
sys.path.insert(0, os.path.dirname(__file__))
import e2e_test as e2e  # 同じフォルダの e2e_test.py の User / Http を使う
from e2e_test import Http, User, check, results

e2e.HOST = os.environ.get("E2E_HOST", "10.40.0.13")
e2e.BASE = f"https://{e2e.HOST}"


async def main():
    ha = Http()
    s, u = ha.call("POST", "/api/login", {"id": "0001", "password": os.environ["E2E_ADMIN_PW"]})
    check("admin ログイン", s == 200, u)

    # ---- CSV テンプレート
    req = __import__("urllib.request").request.Request(e2e.BASE + "/api/admin/users/template.csv")
    with ha.op.open(req) as r:
        tpl = r.read().decode("utf-8")
        cd = r.headers.get("Content-Disposition", "")
    check("テンプレートをダウンロードできる（BOM 付き・見出し）", tpl.startswith("﻿名前,ふりがな,メールアドレス") and "attachment" in cd, tpl[:60])
    check("一般ユーザ・未ログインはテンプレート不可", Http().call("GET", "/api/admin/users/template.csv")[0] == 401)

    # ---- 一括登録
    bad = tpl + "鈴木 花子,すずき はなこ,not-an-email\r\n,なまえなし,\r\n"
    s, r = ha.call("POST", "/api/admin/users/import", {"csv": bad})
    check("不正な行があれば全体を拒否（行番号つき）", s == 400 and "3 行目" in r["error"] and "4 行目" in r["error"], r)
    before = len(ha.call("GET", "/api/admin/users")[1])
    good = tpl + "鈴木 花子,すずき はなこ,hanako@example.com\r\n佐藤 次郎,さとう じろう,\r\n"
    s, r = ha.call("POST", "/api/admin/users/import", {"csv": good, "send_mail": False})
    check("CSV で 2 人登録（記入例の行は除外）", s == 200 and [x["name"] for x in r["users"]] == ["鈴木 花子", "佐藤 次郎"], r)
    after = ha.call("GET", "/api/admin/users")[1]
    check("ユーザ一覧に追加され、初期パスワードも見える", len(after) == before + 2 and after[-1]["initial_password"] == r["users"][-1]["initial_password"])
    hanako = r["users"][0]

    # ---- メール設定
    s, m = ha.call("GET", "/api/admin/mail")
    check("メール設定の取得（パスワードは返さない）", s == 200 and "password" not in m, m)
    s, e = ha.call("PUT", "/api/admin/mail", {"enabled": True, "host": "", "port": 587, "security": "starttls",
                                              "username": "", "password": None, "from_address": "x@example.com", "from_name": "Macross"})
    check("有効化時に SMTP サーバ未入力は拒否", s == 400, e)
    s, _ = ha.call("PUT", "/api/admin/mail", {"enabled": True, "host": "127.0.0.1", "port": 2525, "security": "none",
                                              "username": "u", "password": "secret", "from_address": "macross@example.com", "from_name": "Macross"})
    s2, m = ha.call("GET", "/api/admin/mail")
    check("メール設定の保存（パスワードは保存済み表示）", s == 204 and m["has_password"] and m["host"] == "127.0.0.1", m)
    s, _ = ha.call("PUT", "/api/admin/mail", {**{k: m[k] for k in ["enabled", "host", "port", "security", "username", "from_address", "from_name"]}, "password": None})
    check("パスワード空で保存しても保存済みのものを維持", ha.call("GET", "/api/admin/mail")[1]["has_password"])
    s, e = ha.call("POST", "/api/admin/mail/test", {"to": "test@example.com"})
    check("SMTP に繋がらないときはエラー内容を返す", s == 400 and "送信に失敗" in e["error"], e)
    s, r = ha.call("POST", "/api/admin/users", {"name": "メール失敗確認", "email": "fail@example.com", "send_mail": True})
    check("ID 発行は成功し、メール失敗は結果で知らせる", s == 200 and r["mail"] not in ("sent", "skipped"), r)
    ha.call("PATCH", f"/api/admin/users/{r['id']}", {"disabled": True})
    s, r = ha.call("POST", f"/api/admin/users/{hanako['id']}/reset-password", {"send_mail": True})
    check("リセット時のメール送信（失敗を結果で返す）", s == 200 and r["mail"] not in ("sent", "skipped"), r)
    ha.call("PUT", "/api/admin/mail", {"enabled": False, "host": "", "port": 587, "security": "starttls",
                                       "username": "", "password": None, "from_address": "", "from_name": "Macross"})

    # ---- 管理者の割り込み発言
    hb = Http()
    pw = r["initial_password"]
    hb.call("POST", "/api/login", {"id": hanako["id"], "password": pw})
    hb.call("POST", "/api/password", {"current": pw, "new": "hanako-pass-1"})
    s, c = ha.call("GET", "/api/admin/channels")
    ch1 = {x["name"]: x["id"] for x in c["channels"]}["ch1"]
    ha.call("PATCH", f"/api/admin/channels/{ch1}", {"max_speakers": 1})
    a, b = User("A", ha), User("B", hb)
    await a.open(); await b.open()
    await a.join("L", ch1); await b.join("L", ch1)
    await asyncio.sleep(3)
    await b.send(type="ptt", side="L", on=True)
    m = await b.wait(lambda m: m["type"] == "ptt")
    check("一般ユーザ B が発言開始", m and m["granted"], m)
    st = await a.wait(lambda m: m["type"] == "state" and any(not s["admin"] for c in m["channels"] if c["id"] == ch1 for s in c["speakers"]))
    check("state の発言者に admin フラグ", st is not None)
    await asyncio.sleep(1.5)
    await a.send(type="ptt", side="L", on=True)
    m = await a.wait(lambda m: m["type"] == "ptt")
    check("管理者 A は発言枠が埋まっていても発言できる（割り込み）", m and m["granted"], m)
    m = await b.wait(lambda m: m["type"] == "ptt" and not m["granted"] and m.get("reason"))
    check("B には「管理者が発言するため停止」と通知", m and "管理者" in m["reason"], m)
    st = await a.wait(lambda m: m["type"] == "state" and [s["id"] for c in m["channels"] if c["id"] == ch1 for s in c["speakers"]] == ["0001"])
    check("発言者は管理者だけになる", st is not None)
    await b.send(type="ptt", side="L", on=True)
    m = await b.wait(lambda m: m["type"] == "ptt" and m.get("reason") != None and "発言枠" in m.get("reason", ""))
    check("管理者の発言中に一般ユーザは発言できない", m is not None, b.msgs[-3:])
    await asyncio.sleep(1)
    await a.send(type="ptt", side="L", on=False)
    await asyncio.sleep(1)
    await a.close(); await b.close()
    ha.call("PATCH", f"/api/admin/users/{hanako['id']}", {"disabled": True})
    after = ha.call("GET", "/api/admin/users")[1]
    jiro = [x for x in after if x["name"] == "佐藤 次郎"][0]
    ha.call("PATCH", f"/api/admin/users/{jiro['id']}", {"disabled": True})
    await asyncio.sleep(4)
    print(f"\n結果: {sum(results)}/{len(results)} OK")


asyncio.run(main())
