# Queenfish

여러 Claude 계정의 **아티팩트**와 **사용량(5시간 세션 / 주간 / 모델별 주간 한도)** 을
Maccy처럼 메뉴바 + 전역 단축키 패널로 빠르게 보는 macOS 앱. Rust + Tauri v2. MIT 라이선스.

<p align="center"><img src="docs/screenshot.png" width="460" alt="Queenfish panel" /></p>

## 동작 방식

쿠키/스크래핑 없이, 로컬에 이미 존재하는 **Claude Code OAuth 자격증명**을 그대로 사용한다
(Orca가 사용량을 조회하는 방식과 동일).

| 데이터 | 방법 |
|---|---|
| 자격증명 발견 | macOS Keychain의 `Claude Code-credentials*` 항목 전부(프로필별 접미사 포함) + 각 홈(기본/Orca 프로필/`custom_homes`)의 `.credentials.json` — 액세스 토큰 기준 중복 제거 |
| 계정 식별 | `GET https://api.anthropic.com/api/oauth/profile` → 토큰의 실소유 이메일 (라벨/중복 판정의 기준) |
| 사용량 | `GET https://api.anthropic.com/api/oauth/usage` (`Authorization: Bearer`, `anthropic-beta: oauth-2025-04-20`) |
| 아티팩트 | `https://claude.ai/api/artifacts/mcp` (Claude Code가 쓰는 artifacts MCP 서버의 `list_artifacts` 툴). 내 항목 + 공유받음(`shared_only`) 둘 다 조회, 리스트에 `공유받음` 배지/필터 표시 |
| 토큰 갱신 | Queenfish가 직접 추가한 계정만 만료 시 refresh-token grant로 갱신하고, 새 토큰을 Queenfish 전용 Keychain 항목에 다시 저장. Claude Code/Orca가 저장한 로그인은 **갱신하지 않는다** — refresh token이 1회용이라 대신 갱신하면 그 앱이 로그아웃되기 때문. 만료되면 "토큰 만료"로 표시되고, 해당 계정으로 Claude를 한 번 실행하면 다시 표시된다 |

> 사용량/아티팩트 엔드포인트는 공식 공개 API가 아니라 Claude Code·Orca가 쓰는 내부
> 표면이다. 예고 없이 바뀔 수 있다. 참고:
> [usage 엔드포인트 분석 (dev.to)](https://dev.to/msadofschi/where-claudes-usage-limits-went-from-sse-to-protobuf-frames-c8d),
> [claude usage 커맨드 요청 이슈](https://github.com/anthropics/claude-code/issues/44328)

## 실행

```bash
cd src-tauri
cargo run
```

- 첫 실행 시 Keychain 접근 대화상자가 뜨면 **항상 허용**을 누른다.
- 메뉴바의 `✦` 아이콘, 전역 단축키 **⌘⇧A** 로 패널을 연다.
- 패널: 타이핑으로 검색, `↑↓` 이동, `↵` 열기(기본 브라우저), `⌘C` 링크 복사, `esc` 닫기, `⌘R` 새로고침.
- 선택한 항목의 **미리보기**(제목·설명·종류·계정·수정시각·URL)가 하단에 표시된다.
- 아티팩트 링크를 검색창에 붙여넣고 `↵` 하면 목록에 고정된다 (여러 개 한번에 가능).
- 트레이 메뉴(✦ 우클릭)에서 계정 추가·계정 관리, 패널 `⚙` 버튼으로 관리 모드 토글.
- 데이터는 15분마다, 그리고 패널을 열 때(60초 스로틀) 갱신된다. 429를 만나면
  이전 데이터를 유지한 채 10분 쿨다운한다.

## 계정 추가 / 삭제

- **앱 내 추가**: 패널 하단 `＋` → 기본 브라우저에서 claude.ai 로그인(OAuth PKCE, 본인이 직접
  로그인) → 토큰은 Queenfish 전용 Keychain 항목(`Queenfish-credentials-<email>`)에 저장.
  Claude Code의 Keychain 항목은 절대 건드리지 않는다.
- **삭제**: 사용량 행에 마우스를 올리면 `✕` → 한 번 더 클릭으로 확정.
  - Queenfish로 추가한 계정 → Keychain에서 실제 삭제
  - Claude Code/Orca가 관리하는 계정 → 목록에서 **숨김**만 (로그아웃은 해당 앱에서).
    숨긴 계정은 사용량 영역 하단 칩을 클릭하면 복원.
- **Orca 경유**: Orca에 다른 계정으로 로그인하면(`claude-profiles` 프로필 생성) 자동 인식.
  참고로 Orca는 자체 OAuth 없이 Claude Code 로그인 플로우를 프로필 폴더(HOME 분리)에서
  실행하는 방식이고, Queenfish는 같은 OAuth 클라이언트로 직접 로그인한다.
- **수동**: 별도 디렉터리에 `CLAUDE_CONFIG_DIR=<dir> claude` 로 로그인한 뒤,
  `~/Library/Application Support/com.worxfact.queenfish/config.json` 에 추가:

  ```json
  { "custom_homes": ["/path/to/that/home"] }
  ```

## 번들(.app / .dmg) 만들기

```bash
cd src-tauri
npx @tauri-apps/cli build
# 결과물: src-tauri/target/release/bundle/macos/Queenfish.app
#         src-tauri/target/release/bundle/dmg/Queenfish_0.1.0_aarch64.dmg
```

서명 없이 빌드되므로(ad-hoc) 다른 맥에서 열 때는 우클릭 → 열기가 필요할 수 있다.

## 구조

```
ui/                  정적 프런트엔드 (검색 패널, 사용량 게이지)
src-tauri/src/
  lib.rs             트레이, 전역 단축키, 패널 토글, 새로고침 루프, 커맨드
  accounts.rs        계정 발견 + 자격증명 로드(파일/Keychain)
  api.rs             usage API + artifacts MCP 클라이언트 + 토큰 갱신
```
