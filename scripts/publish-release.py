#!/usr/bin/env python3
"""Publish a stable Review release from master."""

import argparse
import re
import subprocess
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
REPO = "mwinckler/review"
WEB = f"https://github.com/{REPO}"
STABLE = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
MASTER = "bookmarks(exact:master)"
REMOTE_MASTER = "remote_bookmarks(exact:master, exact:origin)"


def run(*args, capture=False, input=None):
    return subprocess.run(
        args,
        cwd=ROOT,
        text=True,
        check=True,
        stdout=subprocess.PIPE if capture else None,
        input=input,
    ).stdout


def jj(*args, capture=False):
    return run(
        "jj", "--color", "never", "--no-pager", "--ignore-working-copy",
        *args, capture=capture,
    )


def revisions(revset):
    return jj("log", "-G", "-r", revset, "-T", r'commit_id ++ "\n"', capture=True).splitlines()


def version_key(version):
    return tuple(map(int, version.split(".")))


def remote_tags():
    rows = jj(
        "tag", "list", "--remote", "origin", "-T",
        r'if(present && !conflict && remote, name ++ "\t" ++ normal_target.commit_id() ++ "\n")',
        capture=True,
    )
    return {
        tag: commit
        for tag, commit in (line.split("\t") for line in rows.splitlines())
        if tag.startswith("v") and STABLE.fullmatch(tag[1:])
    }


def generate_notes(previous, previous_commit, target, tag):
    if not previous:
        raise ValueError("Pass --notes-file with highlights for the first release.")
    subjects = jj(
        "log", "-G", "-r", f"{previous_commit}..{target}", "-T",
        r'if(description, "- " ++ description.first_line() ++ "\n")',
        capture=True,
    ).strip()
    if not subjects:
        raise ValueError(f"Master contains no changes since {previous}.")
    return f"## What changed\n\n{subjects}\n\n[Full changelog]({WEB}/compare/{previous}...{tag})"


def publish(args):
    if not STABLE.fullmatch(args.version):
        raise ValueError("Use a stable version in X.Y.Z form.")
    tag = f"v{args.version}"
    if not args.dry_run:
        remotes = jj("git", "remote", "list", capture=True)
        urls = {
            url + suffix
            for url in (WEB, f"git@github.com:{REPO}", f"ssh://git@github.com/{REPO}")
            for suffix in ("", ".git")
        }
        origin = dict(line.split(maxsplit=1) for line in remotes.splitlines()).get("origin")
        if origin not in urls:
            raise ValueError(f"Point the origin remote at {WEB}.")
        jj(
            "git", "fetch", "--remote", "origin", "--branch", "master", "--tag", "v*",
            "--config", "git.abandon-unreachable-commits=false",
        )

    targets = revisions(MASTER)
    if len(targets) != 1:
        raise ValueError("Resolve the local master bookmark to a single commit.")
    (target,) = targets
    if not args.dry_run and revisions(REMOTE_MASTER) != targets:
        raise ValueError("Sync local master with origin/master before publishing.")
    package = tomllib.loads(jj("file", "show", "-r", target, "Cargo.toml", capture=True))["package"]
    lock = tomllib.loads(jj("file", "show", "-r", target, "Cargo.lock", capture=True))["package"]
    if package["name"] != "review" or package["version"] != args.version or not any(
        entry["name"] == "review" and entry["version"] == args.version and "source" not in entry
        for entry in lock
    ):
        raise ValueError(f"Set the Review version to {args.version} in Cargo.toml and Cargo.lock on master.")

    tags = remote_tags()
    local_tag = revisions(f"tags(exact:{tag})")
    if (local_tag and local_tag != targets) or (tag in tags and tags[tag] != target):
        raise ValueError(f"{tag} already points to another commit.")
    previous = max(
        (name for name in tags if name != tag),
        key=lambda name: version_key(name[1:]),
        default=None,
    )
    if previous and version_key(previous[1:]) >= version_key(args.version):
        raise ValueError(f"Choose a version newer than {previous}.")
    previous_commit = tags.get(previous)
    if previous_commit and not revisions(f"{previous_commit} & ::{target}"):
        raise ValueError(f"Master must contain the previous release, {previous}.")
    if previous_commit == target:
        raise ValueError(f"Master contains no changes since {previous}.")

    if args.notes_file:
        notes = args.notes_file.read_text(encoding="utf-8").strip()
        if not notes:
            raise ValueError("Provide a release notes file with content.")
    else:
        notes = generate_notes(previous, previous_commit, target, tag)
    if args.dry_run:
        print(f"Release {tag} from master ({target}):\n\n{notes}")
        return

    run("gh", "auth", "status", "--hostname", "github.com", "--active")
    if tag not in tags:
        if not local_tag:
            jj("tag", "set", tag, "-r", target)
        jj("git", "push", "--remote", "origin", "--tag", tag)
    run(
        "gh", "release", "create", tag, "--repo", f"github.com/{REPO}", "--verify-tag",
        "--title", f"Review {args.version}", "--notes-file", "-", input=f"{notes}\n",
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="Pass the stable version on master, such as 0.10.0.")
    parser.add_argument("--notes-file", type=Path, help="Use curated Markdown release notes.")
    parser.add_argument("--dry-run", action="store_true", help="Preview the release using local refs.")
    try:
        publish(parser.parse_args())
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Could not publish a Review release: {error}\n")


if __name__ == "__main__":
    main()
