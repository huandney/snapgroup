pub(crate) fn print_cleanup_deferred() {
    println!("snapg boot-clean: outro snapg está rodando; cleanup adiado para o próximo boot");
}

pub(crate) fn print_disarm_failed(error: &anyhow::Error) {
    eprintln!("snapg boot-clean: falha ao desarmar serviço: {error:#}");
}

pub(crate) fn print_no_cleanup_targets() {
    println!("snapg boot-clean: nenhuma sobra pós-restore encontrada");
}

pub(crate) fn print_cleanup_target_removed(name: &str) {
    println!("snapg boot-clean: removido {name}");
}

pub(crate) fn print_cleanup_target_remove_failed(name: &str, error: &anyhow::Error) {
    eprintln!("snapg boot-clean: falha em {name}: {error:#}");
}

pub(crate) fn print_cleanup_summary(ok: usize, total: usize) {
    println!("snapg boot-clean: {ok}/{total} sobras pós-restore removidas");
}
