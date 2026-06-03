# Refinamentos pendentes - doctor / boot recovery

Backlog do fluxo de recuperação de boot. Contexto: o doctor passou a tratar
boot sync interrompido, boot de resgate e a Opção C ("restaurar só o `/` para
casar com o kernel de `/boot`"). O fluxo consertou a máquina real: Opção C
restaurou o `/` para 7.0.10-2, `/boot` casou, `/home` ficou intacto e o sistema
bootou limpo.

## Pendente

### Funcional (combinado)
1. **Kernel-na-lista no restore completo + `snap list`.** Já feito nas telas do
   resgate (diagnóstico + picker da Opção C). Falta no fluxo geral: kernel por
   checkpoint/regret no picker do `restore` e em `snapg list` (ler o kernel do
   membro `root` de cada grupo via `kernel_label`).

### Desenhado na proposta, não construído
2. **"Desfazer" sem reboot** (Parte 2c) — cancelar um restore antes do reboot via
   rename de volta (o `/` vivo ainda é o `_snapg_regret`).
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

1 (fecha o kernel-na-lista) -> 3 (backstop, rede barata). As demais
(2/4/5/6/7) são incrementais, priorizar conforme o uso.
