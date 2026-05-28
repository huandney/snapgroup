❯ snapg restore
Selecione o ponto de restauração: Checkpoint 1777622183 (2026-05-01 03:56:23 — 3 membros) Teste
== RESTAURAR grupo 1777622183 (3 membros) ==
  home: #291  2026-05-01 03:56:23  Teste
  root: #299  2026-05-01 03:56:23  Teste
  root_home: #277  2026-05-01 03:56:24  Teste
Restaurar este checkpoint? (s/N) s
✓ rollback completo do grupo 1777622183 (3 membros)
    home: sistema atual arquivado como @home_snapg_regret
    root: sistema atual arquivado como @_snapg_regret
    root_home: sistema atual arquivado como @root_snapg_regret
Reiniciar agora? (s/N) s
Operation inhibited by "huandney" (PID 2214 "gnome-session-s", user huandney), reason is "user session inhibited".
User huandney is logged in on tty2.
Please retry operation after closing inhibitors and logging out other users.
'systemd-inhibit' can be used to list active inhibitors.
Alternatively, ignore inhibitors and users with 'systemctl reboot -i'.

~ 30s
❯


----------------------
isso ocorreu depois de eu selecionar um instantaneo apos o boot ter dado problema, ele fez o sitema voltar
mas depois apareceu isso 

Snapshot ID     : 303
Date            : 2026-05-01 05:20:44
Description     : pacman --color always -Syu
Restore method  : replace

 Confirm restore of snapshot 303 using the "replace" method?
 Type [y]es to restore, [l]ist to display all snapshots, or [c]ancel to abort.

Your input: l

 ID  │ Date                │ Description
─────┼─────────────────────┼──────────────────────────────────────────────────────────────────────────
 295 │ 2026-04-30 23:36:19 │ zip
 297 │ 2026-05-01 03:51:31 │ /usr/bin/pacman -U /home/huandney/Projetos/snapgroup/snapgroup-0.1.0-1-x
 298 │ 2026-05-01 03:51:36 │ snapgroup
 300 │ 2026-05-01 04:30:39 │ /usr/bin/pacman -U /home/huandney/Projetos/snapgroup/snapgroup-0.1.0-1-x
 301 │ 2026-05-01 04:30:44 │ snapgroup
 302 │ 2026-05-01 04:39:13 │ Tudo Certo
 303 │ 2026-05-01 05:20:44 │ pacman --color always -Syu
 304 │ 2026-05-01 05:22:43 │ bpf cachyos-ananicy-rules cpupower device-mapper ethtool firefox fzf geo

 Which snapshot ID do you want to restore using the "replace" method?
 Use ↑/↓ to select a snapshot ID, or type [c]ancel to abort.

Your input:

cancelei


depois tentei snap restore e obtive:

❯ snapg restore
[sudo] senha para huandney:
Error: snapper list -c root: Erro de E/S (query default id failed, subvolume is not a btrfs subvolume).


~
❯

--------------------


Snapshot detected! Agora mesmo

Restore this snapshot now!

You are currently using this snapshot. Please restore it before rebooting to the normal system.

Restore now

--------
e no btrfs assistents em select root, aparece nada


