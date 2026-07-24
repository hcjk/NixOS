#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandClass {
    Builtin,
    File,
    System,
    Storage,
    Utility,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandDefinition {
    pub name: &'static str,
    pub class: CommandClass,
    pub summary: &'static str,
}

pub const COMMANDS: &[CommandDefinition] = &[
    command("cd", CommandClass::Builtin, "change current directory"),
    command("pwd", CommandClass::Builtin, "print current directory"),
    command(
        "export",
        CommandClass::Builtin,
        "set an environment variable",
    ),
    command(
        "unset",
        CommandClass::Builtin,
        "remove an environment variable",
    ),
    command("help", CommandClass::Builtin, "show commands"),
    command("clear", CommandClass::Builtin, "clear the terminal"),
    command("exit", CommandClass::Builtin, "exit the shell"),
    command("reboot", CommandClass::Builtin, "restart NexOS"),
    command("shutdown", CommandClass::Builtin, "power off NexOS"),
    command("ls", CommandClass::File, "list directory entries"),
    command("cp", CommandClass::File, "copy files"),
    command("mv", CommandClass::File, "move or rename files"),
    command("rm", CommandClass::File, "remove files"),
    command("mkdir", CommandClass::File, "create directories"),
    command("rmdir", CommandClass::File, "remove empty directories"),
    command(
        "touch",
        CommandClass::File,
        "create a file or update its time",
    ),
    command("cat", CommandClass::File, "concatenate files"),
    command("echo", CommandClass::File, "print arguments"),
    command("head", CommandClass::File, "print the first lines"),
    command("tail", CommandClass::File, "print the last lines"),
    command("find", CommandClass::File, "walk a directory tree"),
    command("stat", CommandClass::File, "show file metadata"),
    command("uname", CommandClass::System, "show system identity"),
    command("meminfo", CommandClass::System, "show memory information"),
    command("lspci", CommandClass::System, "list PCI functions"),
    command("lsusb", CommandClass::System, "list USB devices"),
    command("dmesg", CommandClass::System, "show the kernel log"),
    command("date", CommandClass::System, "show wall-clock time"),
    command("uptime", CommandClass::System, "show elapsed time"),
    command("ps", CommandClass::System, "list processes"),
    command("kill", CommandClass::System, "terminate a process"),
    command("free", CommandClass::System, "show memory usage"),
    command("lsblk", CommandClass::Storage, "list block devices"),
    command("mount", CommandClass::Storage, "mount a filesystem"),
    command("umount", CommandClass::Storage, "unmount a filesystem"),
    command("part", CommandClass::Storage, "edit partition tables"),
    command("mkfs.nexfs", CommandClass::Storage, "format NexFS"),
    command("fsck.nexfs", CommandClass::Storage, "check NexFS"),
    command("diskutil", CommandClass::Storage, "manage disks"),
    command("nex-install", CommandClass::Storage, "install NexOS"),
    command("hexdump", CommandClass::Utility, "display bytes"),
    command("wc", CommandClass::Utility, "count lines, words, and bytes"),
    command("sort", CommandClass::Utility, "sort lines"),
    command("sleep", CommandClass::Utility, "wait for a duration"),
    command("edit", CommandClass::Utility, "edit a text file"),
];

const fn command(
    name: &'static str,
    class: CommandClass,
    summary: &'static str,
) -> CommandDefinition {
    CommandDefinition {
        name,
        class,
        summary,
    }
}

#[must_use]
pub fn lookup(name: &[u8]) -> Option<&'static CommandDefinition> {
    COMMANDS
        .iter()
        .find(|command| command.name.as_bytes() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_initial_command_surface() {
        assert_eq!(lookup(b"nex-install").unwrap().class, CommandClass::Storage);
        assert_eq!(lookup(b"echo").unwrap().class, CommandClass::File);
        assert!(lookup(b"not-a-command").is_none());
        assert!(COMMANDS.len() >= 40);
    }
}
