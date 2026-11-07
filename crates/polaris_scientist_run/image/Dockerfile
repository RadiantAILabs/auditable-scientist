# The project image: the research campaign binary on top of the image the
# original system runs in (Debian 13, Python 3.12, pdflatex and bibtex).
#
# Build it with image/build.sh, which stages the build context this file expects:
#
#   polaris-scientist             the release binary
#   configs/circpack_reduced.toml the reduced-budget configuration
#   assets/                       the campaign's prompts, roles and venue notes
#
# The image holds no credential and no copy of the task: credentials reach the
# container as environment variables at run time, and the task folder is mounted
# read-only.
FROM agentic-scientist/agent-cli:latest

USER root

# A wheelhouse for the task's pinned numerical packages, so a run installs them
# with `pip install --no-index --find-links /opt/wheelhouse` and needs the
# network only for a package that is not here. It also holds matplotlib and
# pandas with their dependencies, which a run adds to the task's environment for
# plots and tables (config key evaluation.extra_packages) without touching the
# pinned versions. The build fetches the wheels once, for this image's own
# Python, and checks that they install together offline: the pinned packages
# first, then the plotting packages under the same pins.
RUN python3 -m pip download --no-cache-dir --only-binary=:all: \
        --dest /opt/wheelhouse numpy==2.4.2 scipy==1.17.1 \
        matplotlib==3.11.2 pandas==3.1.1 \
    && printf 'numpy==2.4.2\nscipy==1.17.1\n' > /tmp/wheelhouse-pins.txt \
    && python3 -m venv /tmp/wheelhouse-check \
    && /tmp/wheelhouse-check/bin/python -m pip install --no-cache-dir --no-index \
        --find-links /opt/wheelhouse -r /tmp/wheelhouse-pins.txt \
    && /tmp/wheelhouse-check/bin/python -m pip install --no-cache-dir --no-index \
        --find-links /opt/wheelhouse -c /tmp/wheelhouse-pins.txt \
        matplotlib==3.11.2 pandas==3.1.1 \
    && /tmp/wheelhouse-check/bin/python -I -c 'import matplotlib, pandas, scipy' \
    && rm -rf /tmp/wheelhouse-check /tmp/wheelhouse-pins.txt \
    && chmod -R a+rX /opt/wheelhouse

COPY polaris-scientist /usr/local/bin/polaris-scientist
COPY configs/circpack_reduced.toml /opt/polaris-scientist/configs/circpack_reduced.toml
COPY assets /opt/polaris-scientist/assets
RUN chmod 0755 /usr/local/bin/polaris-scientist \
    && chmod -R a+rX /opt/polaris-scientist

# The campaign finds its prompts, roles and venue notes here. Without this the
# binary would look for the source folder it was built in, which is not in the
# image. Credentials are never set here; the instance metadata service is turned
# off so a missing key is an error and not a lookup on the network.
ENV POLARIS_SCIENTIST_ASSETS=/opt/polaris-scientist/assets \
    AWS_EC2_METADATA_DISABLED=true

# The launcher runs the container as the host user, so nothing here depends on
# the user the base image names.
USER agent
