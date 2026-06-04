# Refinamentos pendentes - doctor / boot recovery

Backlog do fluxo de recuperação de boot. Contexto: o doctor passou a tratar
boot sync interrompido, boot de resgate e a Opção C ("restaurar só o `/` para
casar com o kernel de `/boot`"). O fluxo consertou a máquina real: Opção C
restaurou o `/` para 7.0.10-2, `/boot` casou, `/home` ficou intacto e o sistema
bootou limpo.

## Concluído

- **Kernel-na-lista no restore completo + `snapg list`.** O fluxo geral agora
  mostra o kernel do membro `root` por checkpoint e Regret no picker do
  `restore`, e também em `snapg list`, usando `kernel_label`.
- **Preservação durável do Regret antigo em checkpoint restore.** O Regret
  antigo fica em `.snapgroup_regret_aside` até o próximo boot, e o
  `snapg-cleanup.service` remove esse aside junto das sobras pós-restore. Isso
  prepara o caminho para "desfazer sem reboot" sem depender do terminal.
- **"Desfazer" sem reboot** (Parte 2c). Após checkpoint restore ou restore só
  do `/`, o usuário pode desfazer antes do reboot. O fluxo reverte os renames,
  restaura o Regret antigo preservado em aside e ressincroniza `/boot` quando o
  root participa. O doctor também oferece a ação quando detecta restore
  pendente de reboot.

## Pendente

### Desenhado na proposta, não construído
3. **Backstop antes do reboot** (Fase 3) — invariante "subvol padrão casa com
   `/boot`" como rede universal, independente de como o mismatch surgiu.
4. **Endurecer a janela crítica** — bloquear Ctrl+C / `systemd-inhibit` durante o
   sync, com flag de debug para reprodução.
5. **Recibo `.snapg_boot_sync.json`** (Parte 1 Fase 2) — opcional, baixa
   prioridade.

### Polimento
6. **Cor de paths no resto do app** — `term::path()` aplicado só onde tocamos; o
   rollout global da convenção ficou pendente (sem find-replace cego).
7. **Nuances do picker da Opção C** — "ver todos" (escolher snapshot específico do
   mesmo kernel) e "preferir backup nomeado" (mostrar "Certo" em vez de "—" quando
   o mais recente do kernel não é um backup snapgroup).

## Ordem sugerida

3 (backstop, rede barata). As demais (4/5/6/7) são incrementais, priorizar
conforme o uso.
