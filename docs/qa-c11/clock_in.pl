#!/usr/bin/perl
# clock_in.pl — a Bellman client in a language docs/INTEGRATION.md does not
# cover. Written from that document alone (Connect your own application,
# steps 1-3); no Rust was read and nothing from testing_apps/ was copied.
#
#   perl clock_in.pl <slots-root> <app-name> [seconds-of-work]
#
# Core Perl only: JSON::PP and POSIX have shipped with perl since 5.14.
use strict;
use warnings;
use JSON::PP;
use POSIX qw(strftime);
use File::Spec;

my ($slots, $app, $work) = @ARGV;
die "usage: clock_in.pl <slots-root> <app-name> [work-secs]\n" unless $slots && $app;
$work = 2 unless defined $work;

my $json  = JSON::PP->new->canonical->pretty;
my $fires = File::Spec->catdir($slots, 'fires');
my %seen;                                   # step 3: dedupe by run_id

sub now_utc { strftime('%Y-%m-%dT%H:%M:%SZ', gmtime) }

sub slurp {
    my ($p) = @_;
    open my $fh, '<', $p or return undef;
    local $/;
    my $b = <$fh>;
    close $fh;
    return $b;
}

# Step 2: read the stub, set what changed, write it back atomically
# (temp file + rename onto the same path). One writer, one file.
sub reply {
    my ($path, %fields) = @_;
    my $doc = eval { $json->decode(slurp($path)) } or return 0;
    $doc->{$_} = $fields{$_} for keys %fields;
    my $tmp = "$path.tmp";
    open my $out, '>', $tmp or return 0;
    print {$out} $json->encode($doc);
    close $out;
    rename $tmp, $path or return 0;
    return 1;
}

print "clock_in.pl: watching $fires for app_name=$app\n";
while (1) {
    # Step 1: notice a fire. A plain rescan is a complete implementation.
    opendir(my $dh, $fires) or do { sleep 1; next };
    my @names = sort grep { /^fire-.*\.json$/ } readdir $dh;
    closedir $dh;

    for my $n (@names) {
        my $body = slurp(File::Spec->catfile($fires, $n)) or next;
        my $fire = eval { $json->decode($body) } or next;   # mid-write: try again later
        next unless ($fire->{app_name} // '') eq $app;      # not our work
        my $run = $fire->{run_id} // next;
        next if $seen{$run}++;                              # same firing, act once

        my $path = $fire->{reply_path};                     # absolute — open verbatim
        printf "clock_in.pl: fire run_id=%s kind=%s timer=%s\n",
            $run, $fire->{kind} // '?', $fire->{timer_name} // '?';

        reply($path, state => 'acknowledged', acknowledged_at => now_utc(),
              expected_secs => $work + 0)
            or warn "clock_in.pl: could not write acknowledged\n";

        sleep $work;                                        # ... the actual job ...

        reply($path, state => 'completed', completed_at => now_utc(),
              result => { language => 'perl', worked_secs => $work + 0 })
            or warn "clock_in.pl: could not write completed\n";
        print "clock_in.pl: completed $run\n";
    }
    sleep 1;
}
