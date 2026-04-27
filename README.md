# snapgroup

Wrapper para [Snapper](http://snapper.io/) que agrupa snapshots de múltiplos subvolumes Btrfs (ex: `/` e `/home`) num único ID lógico — `save`, `undo`, `redo`, `delete` operam no grupo inteiro de uma vez.

Binário: `snapg`.

## Por que existe

Snapper trata cada config (`root`, `home`, ...) como universos independentes. Se você quer reverter `/` e `/home` juntos pra um ponto coerente no tempo, tem que correr atrás dos números na unha. `snapgroup` resolve isso amarrando os snapshots via `userdata` (`snapgroup-id=<epoch>`) e oferecendo rollback transacional pareado.

## Requisitos

- Btrfs como filesystem raiz
- Snapper instalado e com pelo menos uma config criada (`snapper -c <nome> create-config <path>`)
- Layout de subvolumes "Snapper-style" — ex: `@`, `@home`, com `.snapshots` montado em cada subvol ativo (padrão openSUSE / CachyOS / instaladores Arch modernos)

## Instalação (Arch / pacman)

Build local do PKGBUILD:

```sh
git clone git@github.com:huandney/snapgroup.git
cd snapgroup
makepkg -si
```

## Comandos

| Comando | O que faz |
|---|---|
| `snapg save [descrição]` | Cria snapshot em todas as configs Snapper, agrupado |
| `snapg list` | Lista grupos existentes (mais recente primeiro) |
| `snapg undo [-y]` | Reverte o grupo mais recente. Exige reboot |
| `snapg redo [-y]` | Desfaz o último `undo` (botão de pânico). Exige reboot |
| `snapg delete [-y]` | Apaga o grupo mais recente |
| `snapg gc [-y]` | Apaga TODOS os subvolumes de backup deixados por undos antigos |

Todos os comandos pedem `sudo` automaticamente (re-exec via `sudo` se não for root).

## Como funciona o `undo` (rollback transacional)

Ao reverter um grupo, para cada membro:

1. Cria cópia writable do snapshot RO (`btrfs subvolume snapshot`)
2. Renomeia subvol ativo → `<nome>_backup_<timestamp>` (rename é metadata-only, atômico; mounts existentes sobrevivem por inode)
3. Renomeia a cópia writable → nome ativo original
4. Move `.snapshots` aninhado de volta pro novo subvol ativo

Se qualquer etapa falhar, rollback é revertido automaticamente (ou com confirmação) — sistema rodando nunca fica num estado inconsistente antes do reboot.

## `redo` (desfazer o `undo`)

Stateless: varre o top-level procurando `<subvol>_backup_<label>`, agrupa pelo label (timestamp ISO, ordem lex = ordem cronológica), restaura o grupo mais recente. Pareamento garantido — só restaura membros do mesmo `label`.

Os subvols vivos pré-redo viram `<subvol>.snapgroup_redo_discard_<label>` (não dá pra deletar enquanto montados). Cleanup é automático via "serviço fantasma":

1. Após `redo` ok, snapg roda `systemctl enable snapg-cleanup.service`.
2. No próximo boot, o systemd executa o serviço, que chama `snapg boot-clean`: apaga todos os `*.snapgroup_redo_discard_*` e em seguida roda `systemctl disable snapg-cleanup.service`.
3. O serviço fica inerte de novo até o próximo redo. Zero overhead em boots normais.

## `gc` (limpeza)

Apaga todos os `<subvol>_backup_<label>` (undo-backups) e qualquer `<subvol>.snapgroup_redo_discard_<label>` remanescente. **Por design, manual e nunca automático para undo-backups** — `gc` automático tornaria `redo` não-confiável. Para `redo-discards` o cleanup automático rola via o serviço descrito acima; `gc` manual continua útil pra limpeza retroativa.

## Status

MVP funcional. Sem testes de integração ainda — usar com `save`/`undo` em par e validar `redo` antes de confiar como botão de pânico.

## Licença

MIT.
